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
//!
//! `enter` on a member row opens the column stage ([`column_fields`]):
//! the family, the parts that family takes, and on a measure family the
//! two glyphs, label and group. Each change folds back into the item's
//! table ([`fold_into`]) and renames the row by its head, so the stage is
//! re-keyed to the new name by its caller.

use std::collections::BTreeSet;

use geode_core::config::{
    Config, Diagnostic, Layer, LayerDoc, Severity, VOL_VIEWS_DOC, merge_docs,
};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::parse_duration;
use geode_core::volboard::delta_text;
use geode_core::volviews::{
    self, ALL_TENORS, Cells, ColumnChoice, CompanionGlyph, DELTAS, Family, LevelGlyph, MeasureText,
    Raw, SPANS, SPARK_30D, TableShape, VolColumnSpec, VolViewSpec, WINDOWS, span_text,
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
        // The document's key, not the view's: written under `default` at the
        // top of the file, so turning it on copies nothing of the view.
        Field {
            key: "default".into(),
            label: "Default".into(),
            kind: FieldKind::Bool(is_default),
            dest: Destination::TopLevel,
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
/// does not model survives. `default` is the document's, not the view's:
/// for [`Destination::TopLevel`] the rendering is the view's name as the
/// key's value when the field is on, and nothing when it is off, which the
/// write path spells as removing the key.
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    if dest == Destination::TopLevel {
        let on = draft
            .fields
            .iter()
            .any(|f| f.key == "default" && f.kind == FieldKind::Bool(true));
        return if on {
            toml_edit::Item::Value(draft.name.as_str().into())
        } else {
            toml_edit::Item::None
        };
    }
    debug_assert_eq!(dest, Destination::Doc);
    let cells = match draft.choice("cells") {
        Some("single") => Cells::Single,
        _ => Cells::Pair,
    };
    let view = VolViewSpec {
        name: draft.name.clone(),
        cells,
        columns: column_specs(draft),
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

/// The view's columns as the writer renders them, in the order the reader
/// numbers them: each member item's own table read as a column. An item
/// whose table is not a column (none the dialog builds) is skipped, so a
/// path index here is the written file's.
fn column_specs(draft: &Draft) -> Vec<VolColumnSpec> {
    draft
        .list_items("columns")
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.source.as_ref().and_then(spec_of))
                .collect()
        })
        .unwrap_or_default()
}

/// Everything the readers would say about the rendered view, against the
/// schema's shapes, plus the one check they cannot make: a name another
/// view already holds. No restart warning: that needs the launch datasets,
/// which only the dialog has ([`validate_with`]).
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    validate_against(draft, config, None)
}

/// [`validate`], plus the one warning only the dialog can raise: a column
/// over a dataset the live schema declares but `startup` — the datasets
/// the data engine was built over at launch — lacks. The catalogue offers
/// such a dataset the moment a reload brings it, but the schema is
/// restart-required, so the column cannot paint until then. A warning,
/// not an error: the write is right, only early. A dataset the live schema
/// lacks too is the resolver's to refuse; a restart would not paint it.
pub fn validate_with(
    draft: &Draft,
    config: &Config,
    startup: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    validate_against(draft, config, Some(startup))
}

fn validate_against(
    draft: &Draft,
    config: &Config,
    startup: Option<&BTreeSet<String>>,
) -> Vec<Diagnostic> {
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
    if let Some(startup) = startup {
        for (i, spec) in column_specs(draft).iter().enumerate() {
            let measure = match spec {
                VolColumnSpec::Measure { measure, .. } | VolColumnSpec::Spark { measure, .. } => {
                    measure
                }
                VolColumnSpec::Vega => continue,
            };
            if let MeasureText::Table { dataset, .. } = measure
                && schema.dataset(dataset).is_some()
                && !startup.contains(dataset)
            {
                diags.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: format!("{dataset} needs a restart to paint"),
                    path: Some(format!("vol_views.{}.columns.{i}", draft.name)),
                });
            }
        }
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

/// The column stage's fields, read from the item's own table: `family`
/// over the catalogue's labels, then only the parts the family takes
/// (`tenor`; `long` and `short`; `window`; `delta`; `span`), then on a
/// measure family the two glyphs — `default` meaning absent in the file —
/// and `label` and `group`, prefilled with the default the tile would show
/// so the row never reads blank. A value the pickers do not offer (a
/// hand-written `9M`, a dataset the schema no longer declares) is added
/// as the row's own option rather than read as the first one, which a
/// later fold would then write.
///
/// A column the pickers cannot hold (`Raw`) has one display-only
/// `measure` row: it stays as it was read, movable and deletable only.
pub fn column_fields(item: &ListItem, families: &[Family]) -> Vec<Field> {
    let dest = Destination::Doc;
    let spec = item.source.as_ref().and_then(spec_of);
    let c = match spec.as_ref().map(ColumnChoice::from_spec) {
        Some(Ok(c)) => c,
        Some(Err(raw)) => {
            return vec![super::views::text_row("measure", "Measure", raw.text, dest)];
        }
        None => {
            return vec![super::views::text_row(
                "measure",
                "Measure",
                item.name.clone(),
                dest,
            )];
        }
    };
    let labels: Vec<String> = families.iter().map(Family::label).collect();
    let mut fields = vec![choice_with("family", "Family", labels, c.family.label())];
    let tenors = || ALL_TENORS.iter().map(|t| t.to_string()).collect::<Vec<_>>();
    // `Some` exactly when the family takes a tenor and the column has one: a
    // dataset keyed by the underlying alone offers none, since the resolver
    // would refuse it.
    if let Some(tenor) = &c.tenor {
        fields.push(choice_with("tenor", "Tenor", tenors(), tenor.clone()));
    }
    if c.family.takes_slope() {
        let long = c.long.clone().unwrap_or_default();
        let short = c.short.clone().unwrap_or_default();
        fields.push(choice_with("long", "Long", tenors(), long));
        fields.push(choice_with("short", "Short", tenors(), short));
    }
    if c.family.takes_window() {
        let windows = WINDOWS.iter().map(|w| w.to_string()).collect();
        let window = c.window.clone().unwrap_or_default();
        fields.push(choice_with("window", "Window", windows, window));
    }
    if c.family.takes_delta() {
        let deltas = DELTAS.iter().map(|d| delta_text(*d)).collect();
        let delta = c.delta.map(delta_text).unwrap_or_default();
        fields.push(choice_with("delta", "Delta", deltas, delta));
    }
    if c.family.takes_span() {
        let spans = SPANS.iter().map(|s| s.to_string()).collect();
        let span = span_text(c.span.unwrap_or(SPARK_30D));
        fields.push(choice_with("span", "Span", spans, span));
    }
    if c.family.takes_glyphs() {
        let level = c.level.map_or("default", LevelGlyph::key);
        let companion = c.companion.map_or("default", CompanionGlyph::key);
        fields.push(choice_with(
            "level",
            "Level",
            glyph_keys(&["plain", "heat", "tint"]),
            level.to_string(),
        ));
        fields.push(choice_with(
            "companion",
            "Companion",
            glyph_keys(&["chip", "bar", "slider"]),
            companion.to_string(),
        ));
        let label = c.label.clone().unwrap_or_else(|| c.default_label());
        let group = c.group.clone().unwrap_or_else(|| c.default_group());
        fields.push(super::views::text_row("label", "Label", label, dest));
        fields.push(super::views::text_row("group", "Group", group, dest));
    }
    fields
}

/// A `Doc` choice selected at `current`, which joins the options when none
/// spells it (the module doc of [`column_fields`] has why).
fn choice_with(key: &str, label: &str, mut options: Vec<String>, current: String) -> Field {
    if !options.contains(&current) {
        options.push(current.clone());
    }
    super::views::choice_row(key, label, options, current, Destination::Doc)
}

/// `default` then the glyph's keys: the file's absence is the first option.
fn glyph_keys(keys: &[&str]) -> Vec<String> {
    std::iter::once("default".to_string())
        .chain(keys.iter().map(|k| k.to_string()))
        .collect()
}

/// Fold a column stage's fields back into `item`: its table is rebuilt from
/// the choice the fields spell and its name becomes the choice's head,
/// de-duplicated against `taken` (the sibling rows' names). Resolves a
/// dataset family by its label alone — see [`fold_into_from`] for the
/// catalogue-aware fold the dialog uses.
pub fn fold_into(item: &mut ListItem, fields: &[Field], taken: &[String]) {
    fold_into_from(item, fields, taken, &[]);
}

/// [`fold_into`] against the view's catalogue (the `columns` list's
/// available rows), which knows a dataset family's exact name and whether
/// its row carries a tenor.
///
/// A `family` change resets the parts whose defaults depend on the family
/// — [`ColumnChoice::new`] for the new one, keeping the old tenor when the
/// new family takes one — and the label and group, which were spelled
/// against the old family's defaults. The glyphs are family-independent set
/// keys and carry across when the new family takes glyphs (a change to a
/// spark or vega column drops them: the reader refuses them there).
/// Otherwise each part the family takes follows its row. A label or group
/// row still showing the text it was prefilled with is unchanged, so it
/// keeps what the file had; typed text is set unless it is empty or equal
/// to the default, which fold to absent.
///
/// A `Raw` column folds nothing: its one row is display-only.
pub(super) fn fold_into_from(
    item: &mut ListItem,
    fields: &[Field],
    taken: &[String],
    catalogue: &[ListItem],
) {
    let Some(old) = item
        .source
        .as_ref()
        .and_then(spec_of)
        .and_then(|s| ColumnChoice::from_spec(&s).ok())
    else {
        return;
    };
    let choice_of = |key: &str| {
        fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::Choice { options, selected } => options.get(*selected).cloned(),
                _ => None,
            })
    };
    let text_of = |key: &str| {
        fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::Text(t) => Some(t.trim().to_string()),
                _ => None,
            })
    };
    let (family, bare) = choice_of("family")
        .map(|label| family_of_label(&label, catalogue))
        .unwrap_or((old.family.clone(), false));
    let mut c = if family == old.family {
        old.clone()
    } else {
        let mut fresh = ColumnChoice::new(family);
        if bare {
            fresh.tenor = None;
        } else if fresh.tenor.is_some() && old.tenor.is_some() {
            fresh.tenor = old.tenor.clone();
        }
        if fresh.family.takes_glyphs() {
            fresh.level = old.level;
            fresh.companion = old.companion;
        }
        return finish(item, &fresh, taken);
    };
    if c.tenor.is_some()
        && let Some(t) = choice_of("tenor")
    {
        c.tenor = Some(t);
    }
    if c.family.takes_slope() {
        if let Some(t) = choice_of("long") {
            c.long = Some(t);
        }
        if let Some(t) = choice_of("short") {
            c.short = Some(t);
        }
    }
    if c.family.takes_window()
        && let Some(w) = choice_of("window")
    {
        c.window = Some(w);
    }
    if c.family.takes_delta()
        && let Some(d) = choice_of("delta")
    {
        c.delta = d.parse().ok().or(c.delta);
    }
    if c.family.takes_span()
        && let Some(s) = choice_of("span")
    {
        c.span = parse_duration(&s).or(c.span);
    }
    if c.family.takes_glyphs() {
        if let Some(l) = choice_of("level") {
            c.level = LevelGlyph::parse(&l);
        }
        if let Some(g) = choice_of("companion") {
            c.companion = CompanionGlyph::parse(&g);
        }
        let shown_label = old.label.clone().unwrap_or_else(|| old.default_label());
        let shown_group = old.group.clone().unwrap_or_else(|| old.default_group());
        c.label = fold_text(
            text_of("label"),
            &shown_label,
            old.label.clone(),
            &c.default_label(),
        );
        c.group = fold_text(
            text_of("group"),
            &shown_group,
            old.group.clone(),
            &c.default_group(),
        );
    }
    finish(item, &c, taken);
}

/// A label or group row's fold: text equal to what the row was prefilled
/// with (`shown`) is unchanged and keeps `prior`; anything else is typed.
/// Empty text and the default both fold to absent.
fn fold_text(
    text: Option<String>,
    shown: &str,
    prior: Option<String>,
    default: &str,
) -> Option<String> {
    let Some(text) = text else {
        return prior;
    };
    let value = if text == shown { prior } else { Some(text) };
    value.filter(|v| !v.is_empty() && v != default)
}

/// Write the choice to the item: its table through the core writer, its
/// name from its head.
fn finish(item: &mut ListItem, c: &ColumnChoice, taken: &[String]) {
    item.source = Some(column_source(&c.to_spec()));
    item.name = item_name(Ok(c), taken);
}

/// The family a Family row's label names, and whether its catalogue row
/// carries no tenor (a dataset keyed by the underlying alone). The
/// catalogue's row is exact; without one the builtin labels decide, and any
/// other label is a dataset's upper-cased name.
fn family_of_label(label: &str, catalogue: &[ListItem]) -> (Family, bool) {
    let listed = catalogue
        .iter()
        .filter(|i| i.name == label)
        .find_map(|i| choice_of_item(i).map(|c| (c.family, c.tenor.is_none())));
    if let Some(found) = listed {
        return found;
    }
    let builtin = Family::builtin_families()
        .into_iter()
        .find(|f| f.label() == label);
    (
        builtin.unwrap_or_else(|| Family::Table(label.to_ascii_lowercase())),
        false,
    )
}

/// The choice a catalogue row (or any item) holds, if the pickers can.
fn choice_of_item(item: &ListItem) -> Option<ColumnChoice> {
    let spec = item.source.as_ref().and_then(spec_of)?;
    ColumnChoice::from_spec(&spec).ok()
}

/// The catalogue's families in its order, read back from its rows — the
/// same list [`catalogue_families`] built them from, without the config.
pub(super) fn families_of(catalogue: &[ListItem]) -> Vec<Family> {
    catalogue
        .iter()
        .filter_map(|i| choice_of_item(i).map(|c| c.family))
        .collect()
}

/// Help for the column stage's fields, under ~90 characters.
pub fn column_help(key: &str) -> &'static str {
    match key {
        "family" => "ATFM, skew, surface point … or a precomputed dataset",
        "tenor" => "The expiry the column reads",
        "long" => "The longer expiry of the slope",
        "short" => "The shorter expiry of the slope",
        "window" => "The realized window in trading days",
        "delta" => "Call delta: 0.25 is the 25Δ call, 0.75 the 25Δ put",
        "span" => "The sparkline's window",
        "level" => "plain, heat (tinted against the column) or tint (the companion's sign)",
        "companion" => "chip, bar (zero bar) or slider (percentile; a chip under z)",
        "label" => "The column head; empty follows the tenor",
        "group" => "The group head; empty follows the family",
        "measure" => "not a measure the pickers know; kept",
        _ => "",
    }
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
    use super::super::{Domain, ObjectDialogState, apply, object_text};
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
        // Without a startup set (a draft built outside the dialog) there is
        // no restart warning; against a startup set that lacks `ssr` the
        // column warns at its index, as a Warning; against one that has
        // it, nothing.
        let restart = |diags: &[Diagnostic]| {
            diags
                .iter()
                .find(|d| d.message == "ssr needs a restart to paint")
                .map(|d| (d.severity, d.path.clone()))
        };
        assert_eq!(restart(&diags), None);
        let lacking: BTreeSet<String> = ["other".to_string()].into();
        let with = validate_with(&draft, &config, &lacking);
        assert_eq!(
            restart(&with),
            Some((Severity::Warning, Some("vol_views.vol.columns.2".into())))
        );
        let having: BTreeSet<String> = ["ssr".to_string()].into();
        assert_eq!(restart(&validate_with(&draft, &config, &having)), None);
        // `Domain::validate` reads the set the dialog hands the draft.
        draft.startup_datasets = Some(lacking);
        assert!(restart(&Domain::VolViews.validate(&draft, &config)).is_some());
    }

    /// A dataset neither the live schema nor startup knows is the
    /// resolver's to refuse; the restart warning would promise what a
    /// restart cannot deliver.
    #[test]
    fn an_unknown_dataset_gets_no_restart_warning() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "vol");
        let FieldKind::OrderedList { items, .. } = &mut draft.fields[3].kind else {
            panic!()
        };
        items.push(ListItem {
            name: "NOWHERE 3M".into(),
            included: true,
            presentation: Default::default(),
            kind: None,
            source: Some("measure = \"nowhere 3M\"".parse().unwrap()),
        });
        let diags = validate_with(&draft, &config, &BTreeSet::new());
        assert!(!diags.is_empty(), "the resolver refuses it");
        assert!(
            !diags.iter().any(|d| d.message.contains("needs a restart")),
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

    /// A `ListItem` for the one `[[columns]]` table `text` spells, named as
    /// the view's list would name it.
    fn item_for(text: &str) -> ListItem {
        let source: toml::Table = text.parse().unwrap();
        let entry = ListItem {
            name: String::new(),
            included: true,
            presentation: Default::default(),
            kind: None,
            source: Some(source),
        };
        ListItem {
            name: added_name(&entry, &[]),
            ..entry
        }
    }

    /// The catalogue with no dataset declared: the twelve builtin families.
    fn no_datasets() -> Vec<Family> {
        Family::builtin_families().to_vec()
    }

    fn select(fields: &mut [Field], key: &str, option: &str) {
        let field = fields.iter_mut().find(|f| f.key == key).unwrap();
        let FieldKind::Choice { options, selected } = &mut field.kind else {
            panic!("{key} is not a choice")
        };
        *selected = options
            .iter()
            .position(|o| o == option)
            .unwrap_or_else(|| panic!("{key} offers no {option}: {options:?}"));
    }

    fn set_text(fields: &mut [Field], key: &str, text: &str) {
        let field = fields.iter_mut().find(|f| f.key == key).unwrap();
        let FieldKind::Text(t) = &mut field.kind else {
            panic!("{key} is not text")
        };
        *t = text.to_string();
    }

    fn measure_spec(text: &str) -> VolColumnSpec {
        VolColumnSpec::Measure {
            measure: volviews::parse_measure(text).unwrap(),
            label: None,
            group: None,
            level: None,
            companion: None,
        }
    }

    fn keys(fields: &[Field]) -> Vec<String> {
        fields.iter().map(|f| f.key.clone()).collect()
    }

    #[test]
    fn the_column_fields_show_only_what_the_family_takes() {
        let atfm = item_for("measure = \"atfm 3M\"");
        assert_eq!(
            keys(&column_fields(&atfm, &no_datasets())),
            ["family", "tenor", "level", "companion", "label", "group"]
        );
        let point = item_for("measure = \"point 3M 0.25\"");
        assert_eq!(
            keys(&column_fields(&point, &no_datasets())),
            [
                "family",
                "tenor",
                "delta",
                "level",
                "companion",
                "label",
                "group"
            ]
        );
        let spark = item_for("spark = \"atfm 3M\"\nspan = \"90d\"");
        assert_eq!(
            keys(&column_fields(&spark, &no_datasets())),
            ["family", "tenor", "span"]
        );
        let vega = item_for("kind = \"vega\"");
        assert_eq!(keys(&column_fields(&vega, &no_datasets())), ["family"]);
        let raw = item_for("measure = \"smile_curv 3M 0.25\"");
        assert_eq!(keys(&column_fields(&raw, &no_datasets())), ["measure"]);
    }

    #[test]
    fn folding_a_family_change_resets_the_parts_and_renames_the_item() {
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "family", "25Δ skew");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "25Δ skew 3M");
        assert_eq!(
            spec_of(item.source.as_ref().unwrap()),
            Some(measure_spec("skew 3M"))
        );
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "companion", "slider");
        select(&mut fields, "level", "heat");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "25Δ skew 3M · heat · slider");
        let s = item.source.as_ref().unwrap();
        assert_eq!(s.get("level").and_then(|v| v.as_str()), Some("heat"));
        assert_eq!(s.get("companion").and_then(|v| v.as_str()), Some("slider"));
    }

    #[test]
    fn a_label_equal_to_its_default_is_not_written() {
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &no_datasets());
        set_text(&mut fields, "label", "3M");
        fold_into(&mut item, &fields, &[]);
        assert!(item.source.as_ref().unwrap().get("label").is_none());
        set_text(&mut fields, "label", "3 months");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(
            item.source
                .as_ref()
                .unwrap()
                .get("label")
                .and_then(|v| v.as_str()),
            Some("3 months")
        );
    }

    /// The label row is prefilled with the default the tile would show; a
    /// step on another part must not turn that prefill into a set label.
    #[test]
    fn a_stepped_tenor_leaves_the_prefilled_label_and_group_unset() {
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &no_datasets());
        assert_eq!(
            fields.iter().find(|f| f.key == "label").map(|f| &f.kind),
            Some(&FieldKind::Text("3M".into()))
        );
        select(&mut fields, "tenor", "6M");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "ATFM 6M");
        let s = item.source.as_ref().unwrap();
        assert!(s.get("label").is_none() && s.get("group").is_none(), "{s}");
        // A typed label survives a later step, and clearing it folds to absent.
        set_text(&mut fields, "label", "half");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "ATFM half");
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "tenor", "1Y");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "ATFM half");
        set_text(&mut fields, "label", "");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "ATFM 1Y");
    }

    /// The glyph choices' `default` is absence in the file. The glyphs are
    /// family-independent: a family change to another measure family keeps
    /// them, while a change to a spark (which takes none) drops them.
    #[test]
    fn default_glyphs_are_absent_and_a_family_change_keeps_them() {
        let mut item = item_for("measure = \"atfm 3M\"\nlevel = \"heat\"\ncompanion = \"bar\"");
        let mut fields = column_fields(&item, &no_datasets());
        assert_eq!(item.name, "ATFM 3M · heat · bar");
        select(&mut fields, "family", "25Δ skew");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "25Δ skew 3M · heat · bar");
        let s = item.source.as_ref().unwrap();
        assert_eq!(s.get("measure").and_then(|v| v.as_str()), Some("skew 3M"));
        assert_eq!(s.get("level").and_then(|v| v.as_str()), Some("heat"));
        assert_eq!(s.get("companion").and_then(|v| v.as_str()), Some("bar"));
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "level", "default");
        fold_into(&mut item, &fields, &[]);
        let s = item.source.as_ref().unwrap();
        assert!(s.get("level").is_none(), "{s}");
        assert_eq!(s.get("companion").and_then(|v| v.as_str()), Some("bar"));
        assert_eq!(item.name, "25Δ skew 3M · bar");
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "family", "Sparkline");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "ATFM 3M · 30d spark");
        let s = item.source.as_ref().unwrap();
        assert!(
            s.get("companion").is_none() && s.get("level").is_none(),
            "{s}"
        );
        assert_eq!(s.get("spark").and_then(|v| v.as_str()), Some("atfm 3M"));
    }

    /// Through the view's catalogue a dataset label resolves to the dataset's
    /// own name, and a dataset keyed by the underlying alone takes no tenor.
    #[test]
    fn a_family_change_to_a_bare_dataset_carries_no_tenor() {
        let bare = "[spot]\nfamily = \"history\"\nkey = [\"underlying_ref\"]\nhistory = \"800d\"\n[spot.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[spot.columns.spot]\ntype = \"f64\"\nrole = \"value\"\n";
        let config = config_with(TWO_TABS, bare);
        let catalogue = catalogue(&config);
        let families = families_of(&catalogue);
        assert_eq!(families, catalogue_families(&config));
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &families);
        select(&mut fields, "family", "SPOT");
        fold_into_from(&mut item, &fields, &[], &catalogue);
        assert_eq!(item.name, "SPOT");
        assert_eq!(
            item.source
                .as_ref()
                .unwrap()
                .get("measure")
                .and_then(|v| v.as_str()),
            Some("spot")
        );
        assert_eq!(
            keys(&column_fields(&item, &families)),
            ["family", "level", "companion", "label", "group"]
        );
        // Without the catalogue the label is still a dataset, by its name.
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &families);
        select(&mut fields, "family", "SPOT");
        fold_into(&mut item, &fields, &[]);
        assert_eq!(item.name, "SPOT 3M");
    }

    #[test]
    fn a_raw_column_folds_nothing_and_a_fold_de_duplicates_the_name() {
        let mut raw = item_for("measure = \"smile_curv 3M 0.25\"");
        let fields = column_fields(&raw, &no_datasets());
        assert_eq!(fields[0].kind, FieldKind::Text("smile_curv 3M 0.25".into()));
        assert!(!text_editable("measure"));
        fold_into(&mut raw, &fields, &["x".into()]);
        assert_eq!(raw.name, "smile_curv 3M 0.25");
        assert_eq!(
            column_help("measure"),
            "not a measure the pickers know; kept"
        );
        let mut item = item_for("measure = \"atfm 3M\"");
        let mut fields = column_fields(&item, &no_datasets());
        select(&mut fields, "tenor", "1W");
        fold_into(&mut item, &fields, &["ATFM 1W".into()]);
        assert_eq!(item.name, "ATFM 1W ·2");
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

    /// The `default` field is the document's own key: on, it renders the
    /// view's name as a bare value under the field's key; off, it renders
    /// nothing, which the top-level destination spells as a removal.
    #[test]
    fn the_default_field_writes_the_top_level_key_and_clears_it_when_off() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "skew");
        assert_eq!(draft.fields[2].key, "default");
        assert_eq!(draft.fields[2].dest, Destination::TopLevel);
        draft.fields[2].kind = FieldKind::Bool(true);
        let item = Domain::VolViews.to_table(&draft, Destination::TopLevel);
        assert!(matches!(
            apply::object_value("default", item, Destination::TopLevel),
            apply::ObjectWrite::Set(toml::Value::String(s)) if s == "skew"
        ));
        draft.fields[2].kind = FieldKind::Bool(false);
        let item = Domain::VolViews.to_table(&draft, Destination::TopLevel);
        assert!(matches!(
            apply::object_value("default", item, Destination::TopLevel),
            apply::ObjectWrite::Remove
        ));
        // The view's own table never carries the document's key, whatever
        // the field says.
        draft.fields[2].kind = FieldKind::Bool(true);
        let text = object_text("skew", Domain::VolViews.to_table(&draft, Destination::Doc));
        assert!(!text.contains("default"), "{text}");
    }

    /// `shift+k` on the second tab swaps it with the first and reports the
    /// whole new order; on the first tab there is nowhere to go.
    #[test]
    fn move_object_swaps_neighbours_and_stops_at_the_ends() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut state = ObjectDialogState::new(Domain::VolViews);
        state.refresh_rows(&config, (1, 0), Vec::new());
        state.selected = 1;
        assert_eq!(
            state.move_object(-1),
            Some(vec!["skew".to_string(), "vol".to_string()])
        );
        assert_eq!(state.selected, 0);
        assert_eq!(
            state
                .rows
                .rows()
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["skew", "vol"],
            "the list shows the move at once"
        );
        assert_eq!(state.move_object(-1), None);
        assert_eq!(state.selected, 0);
        state.selected = 1;
        assert_eq!(state.move_object(1), None);
    }

    /// Every other domain's browse list stays put under `shift+j`.
    #[test]
    fn only_vol_views_reorders_from_browse() {
        assert!(Domain::VolViews.reorderable());
        for domain in Domain::ALL {
            if domain != Domain::VolViews {
                assert!(!domain.reorderable(), "{domain:?}");
            }
        }
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut state = ObjectDialogState::new(Domain::Views);
        state.refresh_rows(&config, (1, 0), Vec::new());
        assert_eq!(state.move_object(1), None);
    }
}
