//! The `Domain::Scopes` adapter (spec §8.4, reversed by
//! `docs/superpowers/specs/2026-09-19-geode-scopes-dialog-editing-design.md`
//! from a read-only summary to a real editor): the scopes a trader saves
//! with `:scope save <name>` and recalls with `:scope load <name>` or the
//! palette's `scope::<name>` actions.
//!
//! The adapter edits all three keys a saved [`Scope`] has: `dimensions`
//! as a [`FieldKind::OrderedList`] (one item per non-empty selection,
//! `crate::shell::pickable_columns` as what else may join it — `enter`
//! opens the Values stage to tick a selection's own values, Task 3), and
//! `text`/`expression` as `i`-editable [`FieldKind::Text`] rows
//! ([`parse_text`] refuses a broken expression with `parse_expr`'s own
//! message rather than writing it for the loader to warn about and
//! drop). [`to_table`] still renders `draft.source` and nothing else —
//! [`fold`] (every text or dimensions-list change) and `fold_values`
//! (the Values stage's own fold, Task 3) are its only writers, so every
//! keystroke reaches `source` before the validator or the writer ever
//! sees it.
//!
//! **No `as-of` field.** A saved [`Scope`] carries no as-of at all —
//! `Frame::save_scope` saves `self.scope.clone()` alone
//! (`geode-shell/src/frame.rs`), and `scope_to_table`'s own three keys
//! (`dimensions`, `text`, `expression`) have no fourth. As-of is frame
//! state, not scope state; showing one here would be inventing a field
//! nothing writes.
//!
//! ## `o`: the one door onto a `Frame`
//!
//! Everywhere else in this module is pure — `o` is the one verb that
//! needs a `Frame`, and it is read in `render.rs`, never here.
//! `render.rs`'s `Verb('o')` arm (`arm_overwrite`) only *arms* the
//! confirm — it decides at that moment whether the write will also fork
//! the object (whether the user layer already owns this name), so the
//! prompt can disclose it, but it reads no `Frame`. The frame is read a
//! keystroke later, in `run_confirmed`'s `Confirm::Overwrite` arm: `shell.
//! frame.read(cx).scope().clone()` — the only place a `Frame` is read
//! for *this dialog* at all (narrower than "the only entity": the shared
//! filter field's `Entity<InputState>` is read elsewhere same as in every
//! other dialog). [`overwrite_with`] takes the resulting `Scope` value,
//! already read out, plus the live `Config` for the available block, so
//! this module, like every other adapter, never touches a `Frame` or
//! `gpui` itself.
//!
//! It replaces both `draft.source` (what [`to_table`] renders) and
//! `draft.fields` (the read-only summary painted above it) with the
//! frame's own scope. Only `source` is what `Draft::is_dirty` and
//! `Draft::writes_by_destination` actually key the write decision on —
//! comparing the two *painted* fields alone would make "did anything
//! change" a question about whether `selects_summary` happens to render
//! two different scopes identically, which is not a property it
//! promises to keep as it grows (see `Draft::baseline_source`'s own doc
//! in `mod.rs`). Rebuilding `fields` here is still necessary — nothing
//! else repaints them — it just is not what decides whether `o` writes.

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::scopes::{saved_scopes_from_doc, scope_to_table};

use super::ListItem;
use super::{Destination, Draft, Field, FieldKind};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "scopes";

/// The muted second line of a scope's browse row: what it selects, in
/// the same "column ∈ values" spelling the scope bar's own chips use
/// (`scopebar::build_model`) — except never collapsed to a bare count
/// the way a chip is past two values, because a browse row has the
/// whole width of the dialog and nothing else competing for it.
///
/// Read straight off the raw TOML value rather than through
/// `saved_scopes_from_doc`, for the reason `views::summary` and
/// `groupings::summary` both give for doing the same: a malformed scope
/// is exactly the one a user opens this dialog to fix, and the reader
/// drops such a scope from the merged result entirely, which would leave
/// the row with nothing to show.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    selects_summary(table)
}

/// What a scope's raw table selects: one "column ∈ values" clause per
/// non-empty dimension, plus the expression's own source text when it
/// has one. [`summary`]'s whole body — the browse row's own spelling,
/// kept collapsed to this one line even though the edit stage now shows
/// the same selections as a real list, because a browse row has no
/// spare width for anything past a summary.
fn selects_summary(table: &toml::Table) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(dims) = table.get("dimensions").and_then(|v| v.as_table()) {
        for (column, values) in dims {
            let values: Vec<&str> = values
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if values.is_empty() {
                continue;
            }
            parts.push(format!("{column} ∈ {}", values.join(", ")));
        }
    }
    if let Some(expr) = table
        .get("expression")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        parts.push(expr.to_string());
    }
    if parts.is_empty() {
        "everything".to_string()
    } else {
        parts.join("; ")
    }
}

/// The values a scope's raw table selects on `column`, in file order.
fn values_of(table: Option<&toml::Table>, column: &str) -> Vec<String> {
    table
        .and_then(|t| t.get("dimensions"))
        .and_then(|v| v.as_table())
        .and_then(|d| d.get(column))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What a selection's row says after its column name: the values
/// themselves up to three, a count past that — the same "show it, then
/// count it" rule a view's `column_summary` and a grouping slot's chain
/// both follow, kept short because a values note shares its row with
/// the column name and the tick.
pub fn dimension_note(values: &[String]) -> String {
    if values.len() <= 3 {
        values.join(", ")
    } else {
        format!("{} values", values.len())
    }
}

/// The scope's three fields (spec §3): its selected dimensions as a list
/// (one item per non-empty selection, the other pickable columns
/// available to join it), then `text` and `expression` as editable
/// text — or of no scope at all when `object` names nothing, which is
/// what a `Config` with no `scopes` doc, or a slot nothing defines, has
/// to produce rather than panicking (spec §4 has `fields` serve the
/// create path too, though Scopes has no create verb of its own — see
/// this module's own doc comment).
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(config, table)
}

/// The shared body of [`fields`] and [`overwrite_with`]: both need the
/// same three rows built from a raw scope table and the live `Config`
/// (for the dimensions list's available catalogue), one read off
/// `Config` itself, the other freshly rendered from the frame's own
/// scope.
fn fields_from_table(config: &Config, table: Option<&toml::Table>) -> Vec<Field> {
    let mut items = Vec::new();
    if let Some(dims) = table
        .and_then(|t| t.get("dimensions"))
        .and_then(|v| v.as_table())
    {
        for (column, _) in dims {
            let values = values_of(table, column);
            if values.is_empty() {
                continue;
            }
            items.push(ListItem {
                name: column.clone(),
                included: true,
                presentation: Default::default(),
                kind: None,
                note: Some(dimension_note(&values)),
            });
        }
    }
    let available: Vec<ListItem> = crate::shell::pickable_columns(config)
        .into_iter()
        .filter(|p| !items.iter().any(|i| i.name == p.column))
        .map(|p| ListItem {
            name: p.column,
            included: false,
            presentation: Default::default(),
            kind: None,
            note: None,
        })
        .collect();
    let text = |key: &str| {
        table
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    vec![
        Field {
            key: "dimensions".to_string(),
            label: "Dimensions".to_string(),
            kind: FieldKind::OrderedList {
                items,
                available: Some(available),
            },
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "text".to_string(),
            label: "Text filter".to_string(),
            kind: FieldKind::Text(text("text")),
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "expression".to_string(),
            label: "Expression".to_string(),
            kind: FieldKind::Text(text("expression")),
            dest: Destination::Doc,
            layer: None,
        },
    ]
}

/// The notice `shift+j`/`shift+k` answer on this domain (ruling 4, spec
/// §3.2): a scope's selections have no meaningful order — the compiler
/// reads them as a set, and `render.rs`'s reorder key wires this in
/// (Task 4) — kept here so both the notice's wording and the rule that
/// produces it live in one place. `render.rs` does not read it yet, so
/// it is unreached until that wiring lands — this crate's own
/// `dead_code` lint would otherwise fail `-D warnings` on this task
/// alone for a constant Task 4 is contracted to consume.
#[allow(dead_code)]
pub const NO_ORDER_NOTICE: &str = "selections have no order";

/// The draft rendered as `scopes.toml`'s own value for this object:
/// `draft.source`, unchanged, wrapped for [`super::Domain::to_table`]'s
/// sake.
///
/// Never derived from `draft.fields` — both fields are
/// [`FieldKind::Text`], painted summaries with nothing in the field
/// vocabulary that could reconstruct a `dimensions` sub-table or an
/// `expression` string from them. `draft.source` is the actual object,
/// exactly as `Domain::draft` first read it off the merged doc, and the
/// **only** thing that ever changes it is [`overwrite_with`] — the same
/// "the field vocabulary does not model this, so render from `source`"
/// shape `views::doc_table`'s own doc comment describes, taken all the
/// way to its limit: here, nothing but `source` is ever rendered.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
}

/// Everything wrong with the draft as it stands (spec §7.2): the
/// rendered table, parsed back and read by exactly the reader that
/// decides which saved scopes `:scope load` and the palette's
/// `scope::<name>` actions can reach (`saved_scopes_from_doc`) — on the
/// object being edited alone, wrapped in a document of its own, for the
/// reason `views::validate` and `groupings::validate` both give for doing
/// the same: validating the whole merged doc would report every other
/// scope's problems against this one object.
///
/// `saved_scopes_from_doc` only ever produces `Severity::Warning`
/// diagnostics (a scope naming an unknown column, or an unparseable
/// expression, is dropped with a warning, never an error) — so this
/// adapter's `o` can never be refused by `apply::blocking_diagnostic`,
/// which only gates on `Severity::Error`.
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
    let (_saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
    diags
}

/// The draft's `scopes.toml` entry, rendered and parsed back the way the
/// loader would read it off disk.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

/// `o`, confirmed: overwrite `draft` with `scope`'s own contents.
///
/// `scope` is a plain value, not a `Frame` — `render.rs`'s
/// `run_confirmed` reads `shell.frame.read(cx).scope().clone()` at the
/// call site and hands the result in here, which is what keeps a gpui
/// `Entity` out of this module (and out of `Domain`'s whole surface)
/// entirely; `config` is the same `shell.services.config` the caller
/// already has in hand, needed here only to rebuild the dimensions
/// list's available catalogue. Both `draft.source` (what [`to_table`]
/// renders) and `draft.fields` (the rows painted above it) are
/// replaced —
/// `draft.source` is what `Draft::is_dirty` and
/// [`super::Draft::writes_by_destination`] key the actual write decision
/// on, so `apply::commit_edit` sees a change regardless of what the
/// painted summary says, and goes through that same door every other
/// field edit already does.
pub fn overwrite_with(draft: &mut Draft, scope: &Scope, config: &Config) {
    draft.source = scope_table_as_toml(scope);
    draft.fields = fields_from_table(config, Some(&draft.source));
}

/// `scope_to_table` (geode-core) as the `toml::Table` `Draft::source`
/// holds: an object's own inner table, not the object wrapped under its
/// own name.
///
/// Round-tripped through [`super::object_text`] — the one spelling of
/// what a flush actually produces — rather than hand-converted, for the
/// same reason `groupings::rendered_doc_table` goes through a real
/// parse instead of trusting a bespoke conversion: the one honest check
/// of what `saved_scopes_from_doc` will read back is the text a write
/// would really contain.
fn scope_table_as_toml(scope: &Scope) -> toml::Table {
    let text = super::object_text("scope", toml_edit::Item::Table(scope_to_table(scope)));
    text.parse::<toml::Table>()
        .ok()
        .and_then(|parsed| parsed.get("scope").and_then(|v| v.as_table()).cloned())
        .unwrap_or_default()
}

/// `i`'s commit door (spec §5, [`super::Domain::parse_text`]): `text`
/// trims; `expression` must parse — a broken expression is refused with
/// the parser's own message rather than written for the loader to warn
/// about and drop, since `saved_scopes_from_doc` only ever reports an
/// unparseable expression as a warning and silently drops it, which
/// would otherwise make a typo in this dialog look like it saved.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if key == "expression" && !text.is_empty() {
        geode_core::scope::parse_expr(text).map_err(|e| format!("expression: {e}"))?;
    }
    Ok(text.to_string())
}

/// Fields → `source` (spec §3): the two text keys as typed, and
/// `dimensions` retained to the columns the list still names. A kept
/// selection's VALUES are never rewritten here — the Values stage owns
/// them (`fold_values`, Task 3). Called from `render::revalidate` on
/// every Scopes change, ahead of the validator and the writer.
pub fn fold(draft: &mut Draft) {
    let mut text = None;
    let mut expression = None;
    // `None` while the `dimensions` field is not installed — the Values
    // stage has stashed it — so the fold never retains against an empty
    // list and wipes every selection (the trap CLAUDE.md records).
    let mut kept: Option<Vec<String>> = None;
    for field in &draft.fields {
        match (field.key.as_str(), &field.kind) {
            ("text", FieldKind::Text(t)) => text = Some(t.clone()),
            ("expression", FieldKind::Text(e)) => expression = Some(e.clone()),
            ("dimensions", FieldKind::OrderedList { items, .. }) => {
                kept = Some(items.iter().map(|i| i.name.clone()).collect());
            }
            _ => {}
        }
    }
    if let Some(text) = text {
        draft
            .source
            .insert("text".into(), toml::Value::String(text));
    }
    if let Some(expression) = expression {
        draft
            .source
            .insert("expression".into(), toml::Value::String(expression));
    }
    if let Some(kept) = kept {
        let dims = draft
            .source
            .entry("dimensions".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(dims) = dims.as_table_mut() {
            dims.retain(|column, _| kept.iter().any(|k| k == column));
        }
    }
}

/// The Values stage's one field before the data answers (spec §4): a
/// display-only row, so the stage has a shape to paint and `escape` to
/// leave by. Replaced whole by [`values_fields`] on delivery.
///
/// `render::enter_values_stage` (Task 4) is what seeds a fresh Values
/// stage with this before the `Request::Distinct` round trip lands —
/// unreached until that door exists, so `#[allow(dead_code)]` for the
/// same reason [`NO_ORDER_NOTICE`] carries it.
#[allow(dead_code)]
pub fn loading_field() -> Vec<Field> {
    status_field("loading…")
}

/// The Values stage's one field when the request failed: the failure
/// text on the row, `escape` the way out.
pub fn failed_field(message: &str) -> Vec<Field> {
    status_field(message)
}

fn status_field(text: &str) -> Vec<Field> {
    vec![Field {
        key: "values".to_string(),
        label: "Values".to_string(),
        kind: FieldKind::Text(text.to_string()),
        dest: Destination::Doc,
        layer: None,
    }]
}

/// The Values stage's list (spec §4): one row per delivered `(value,
/// count)` in the outcome's own order, ticked iff `saved` lists it, the
/// count as its note; then every saved value the data does NOT hold,
/// ticked, noted `not in data` (ruling 5) — visible and untickable,
/// never silently dropped.
pub fn values_fields(saved: &[String], values: &[(String, u64)]) -> Vec<Field> {
    let mut items: Vec<ListItem> = values
        .iter()
        .map(|(value, count)| ListItem {
            name: value.clone(),
            included: saved.iter().any(|s| s == value),
            presentation: Default::default(),
            kind: None,
            note: Some(count.to_string()),
        })
        .collect();
    for value in saved {
        if !values.iter().any(|(v, _)| v == value) {
            items.push(ListItem {
                name: value.clone(),
                included: true,
                presentation: Default::default(),
                kind: None,
                note: Some("not in data".to_string()),
            });
        }
    }
    vec![Field {
        key: "values".to_string(),
        label: "Values".to_string(),
        kind: FieldKind::OrderedList {
            items,
            available: None,
        },
        dest: Destination::Doc,
        layer: None,
    }]
}

/// The Values stage's fold (spec §4): the ticked values become
/// `source.dimensions.<column>` — the key removed outright when none is
/// ticked, since an empty array is never written — and the stashed
/// parent's `dimensions` list follows: a first tick inserts the item
/// (out of the available block), an emptied selection returns it there,
/// a changed selection refreshes the note. Called from
/// `render::revalidate` on every tick while [`Draft::values`] is `Some`;
/// a no-op while the stage still shows its loading/failed row.
pub fn fold_values(draft: &mut Draft) {
    let Some(column) = draft.values().map(str::to_string) else {
        return;
    };
    let Some(items) = draft.list_items("values") else {
        return;
    };
    let ticked: Vec<String> = items
        .iter()
        .filter(|i| i.included)
        .map(|i| i.name.clone())
        .collect();
    let dims = draft
        .source
        .entry("dimensions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(dims) = dims.as_table_mut() {
        if ticked.is_empty() {
            dims.remove(&column);
        } else {
            dims.insert(
                column.clone(),
                toml::Value::Array(ticked.iter().cloned().map(toml::Value::String).collect()),
            );
        }
    }
    draft.with_parent_list("dimensions", |items, available| {
        let position = items.iter().position(|i| i.name == column);
        match (position, ticked.is_empty()) {
            (Some(i), true) => {
                let mut entry = items.remove(i);
                entry.included = false;
                entry.note = None;
                if let Some(available) = available {
                    available.push(entry);
                }
            }
            (Some(i), false) => items[i].note = Some(dimension_note(&ticked)),
            (None, false) => {
                let mut entry = available
                    .as_mut()
                    .and_then(|a| a.iter().position(|i| i.name == column).map(|p| a.remove(p)))
                    .unwrap_or_else(|| ListItem {
                        name: column.clone(),
                        included: true,
                        presentation: Default::default(),
                        kind: None,
                        note: None,
                    });
                entry.included = true;
                entry.note = Some(dimension_note(&ticked));
                items.push(entry);
            }
            (None, true) => {}
        }
    });
}

/// The draft's scope as `saved_scopes_from_doc` would read it, with
/// `minus`'s own selection removed — what the distinct request carries
/// (spec §4), so a value's count answers "within the scope I am
/// authoring". An unreadable draft (the reader warns and drops it)
/// yields the empty scope: the counts are then dataset-wide, which is
/// honest for a scope that does not yet parse.
///
/// `render::enter_values_stage` (Task 4) is the one caller — the
/// non-test build has none yet, so `#[allow(dead_code)]` for
/// [`loading_field`]'s own reason.
#[allow(dead_code)]
pub fn draft_scope(draft: &Draft, config: &Config, minus: &str) -> Scope {
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
    let (mut saved, _) = saved_scopes_from_doc(&doc, &schema, &dims);
    let mut scope = saved.remove(&draft.name).unwrap_or_default();
    scope.dimensions.retain(|d| d.column != minus);
    scope
}

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)).
pub fn help(key: &str) -> &'static str {
    match key {
        "dimensions" => {
            "The dimensions this scope narrows — open one to tick its values, x drops it"
        }
        "text" => "A text filter matched against every textual column; empty for none",
        "expression" => {
            "A filter expression over the scope's columns, checked when applied; empty for none"
        }
        "values" => "The values this dimension keeps — every row counts what the scope would leave",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, Step};
    use super::*;
    use geode_core::config::ConfigSources;
    use geode_core::scope::DimensionSelection;

    /// A whole-document fixture (`[dimensions]\n...`), wrapped as the
    /// `toml::Value::Table` a scope's row actually is — `toml::Value`'s
    /// own `FromStr` parses one bare value expression, not a document
    /// with table headers, so this goes through `toml::Table` the same
    /// way `views`'s own test fixtures do.
    fn value(text: &str) -> toml::Value {
        toml::Value::Table(text.parse::<toml::Table>().expect("fixture value parses"))
    }

    #[test]
    fn the_summary_names_every_dimension_selection() {
        assert_eq!(
            summary(&value(
                "[dimensions]\nbook = [\"BK000\", \"BK003\", \"BK007\"]\n"
            )),
            "book ∈ BK000, BK003, BK007"
        );
    }

    /// A malformed scope is exactly the one a user opens the dialog to
    /// fix, so it still gets a row and the row still says what is wrong
    /// — the same rule `views::summary`'s and `groupings::summary`'s own
    /// tests pin.
    #[test]
    fn a_malformed_scope_still_describes_itself() {
        assert_eq!(summary(&toml::Value::String("oops".into())), "not a table");
    }

    /// An empty scope (no dimensions, no text, no expression) selects
    /// everything, and says so rather than showing a blank row.
    #[test]
    fn an_empty_scope_says_everything() {
        assert_eq!(summary(&value("[dimensions]\n")), "everything");
    }

    fn config_with_scope(scopes: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\ncategorical = true\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let scopes = LayerDoc::builtin("scopes", scopes).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, scopes],
            desk: None,
            user: None,
        })
    }

    /// Three fields: the selected dimensions as a list with the other
    /// pickable columns available, then the text filter and the
    /// expression as editable text.
    #[test]
    fn fields_are_the_dimensions_list_the_text_filter_and_the_expression() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\nexpression = \"npv > 0\"\n[mine.dimensions]\nbook = [\"BK001\", \"BK003\"]\n\n[bare]\n",
        );
        let fields = Domain::Scopes.fields(&config, Some("mine"));
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].key, "dimensions");
        let FieldKind::OrderedList { items, available } = &fields[0].kind else {
            panic!("dimensions is a list");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "book");
        assert!(items[0].included);
        assert_eq!(items[0].note.as_deref(), Some("BK001, BK003"));
        // `book` is selected; `lhu` (the fixture's other categorical
        // column) stays in the available catalogue.
        let available_names: Vec<&str> = available
            .as_ref()
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(available_names, ["lhu"]);
        assert_eq!(fields[1].key, "text");
        assert_eq!(fields[1].kind, FieldKind::Text("spx".to_string()));
        assert_eq!(fields[2].key, "expression");
        assert_eq!(fields[2].kind, FieldKind::Text("npv > 0".to_string()));

        let bare = Domain::Scopes.fields(&config, Some("bare"));
        let FieldKind::OrderedList { items, available } = &bare[0].kind else {
            panic!("dimensions is a list");
        };
        assert!(items.is_empty());
        let bare_names: Vec<&str> = available
            .as_ref()
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(bare_names, ["book", "lhu"]);
        assert_eq!(bare[1].kind, FieldKind::Text(String::new()));
        assert_eq!(bare[2].kind, FieldKind::Text(String::new()));
    }

    #[test]
    fn an_object_that_does_not_exist_has_the_empty_fields_rather_than_panicking() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Scopes.fields(&config, object);
            assert!(
                matches!(&fields[0].kind, FieldKind::OrderedList { items, .. } if items.is_empty())
            );
            assert_eq!(fields[1].kind, FieldKind::Text(String::new()));
        }
    }

    /// The note after a selection's name: the values up to three, then a
    /// count.
    #[test]
    fn a_dimension_note_lists_up_to_three_values_then_counts() {
        assert_eq!(dimension_note(&["BK001".into()]), "BK001");
        assert_eq!(
            dimension_note(&["A".into(), "B".into(), "C".into()]),
            "A, B, C"
        );
        assert_eq!(
            dimension_note(&["A".into(), "B".into(), "C".into(), "D".into()]),
            "4 values"
        );
    }

    /// `i` opens `text` and `expression`; a bad expression is refused
    /// with the parser's own message, an empty one clears the key.
    #[test]
    fn text_and_expression_are_editable_and_the_expression_is_parsed() {
        assert!(Domain::Scopes.text_editable("text"));
        assert!(Domain::Scopes.text_editable("expression"));
        assert!(!Domain::Scopes.text_editable("dimensions"));
        assert_eq!(
            Domain::Scopes.parse_text("expression", " npv > 0 "),
            Ok("npv > 0".to_string())
        );
        assert_eq!(
            Domain::Scopes.parse_text("expression", ""),
            Ok(String::new())
        );
        let err = Domain::Scopes
            .parse_text("expression", "npv >")
            .unwrap_err();
        assert!(err.starts_with("expression: "), "{err}");
        assert_eq!(
            Domain::Scopes.parse_text("text", " spx "),
            Ok("spx".to_string())
        );
    }

    /// `fold` writes the two text fields into `source` and drops a
    /// dimension the list no longer names; it never touches a selection's
    /// values (those belong to the Values stage).
    #[test]
    fn fold_writes_text_expression_and_retained_dimensions_into_source() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\"]\nlhu = [\"L1\"]\n",
        );
        let mut draft = Domain::Scopes.draft(&config, "mine");
        // Type a new filter and an expression.
        draft.fields[1].kind = FieldKind::Text("ndx".to_string());
        draft.fields[2].kind = FieldKind::Text("npv > 0".to_string());
        // Drop `lhu` from the list, as `x` does.
        if let FieldKind::OrderedList { items, .. } = &mut draft.fields[0].kind {
            items.retain(|i| i.name != "lhu");
        }
        fold(&mut draft);
        assert_eq!(draft.source["text"].as_str(), Some("ndx"));
        assert_eq!(draft.source["expression"].as_str(), Some("npv > 0"));
        let dims = draft.source["dimensions"].as_table().unwrap();
        assert!(dims.contains_key("book"));
        assert!(!dims.contains_key("lhu"));
        assert_eq!(
            dims["book"].as_array().unwrap()[0].as_str(),
            Some("BK001"),
            "the fold never rewrites a kept selection's values"
        );
        assert!(draft.is_dirty());
    }

    /// While the Values stage has stashed the `dimensions` field, `fold`
    /// leaves `source.dimensions` alone rather than retaining it against
    /// an empty list.
    #[test]
    fn fold_leaves_dimensions_alone_while_the_values_stage_is_open() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values("book", Vec::new()));
        fold(&mut draft);
        assert!(
            draft.source["dimensions"]
                .as_table()
                .unwrap()
                .contains_key("book")
        );
    }

    /// `to_table` renders `draft.source` verbatim — nothing in the field
    /// vocabulary feeds it, so a draft opened straight off a scope's own
    /// table round-trips through `saved_scopes_from_doc` unchanged (the
    /// round trip `Domain::validate` leans on).
    #[test]
    fn to_table_round_trips_through_saved_scopes_from_doc() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\", \"BK002\"]\n",
        );
        let draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.diagnostics.is_empty(), "{:?}", draft.diagnostics);
        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("mine", item);
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
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["mine"].text.as_deref(), Some("spx"));
        assert_eq!(
            saved["mine"].dimensions[0].values,
            vec!["BK001".to_string(), "BK002".to_string()]
        );
    }

    /// [`overwrite_with`]: the frame's scope replaces both the source
    /// `to_table` renders and the fields painted above it, and the two
    /// stay consistent with each other — the same object `fields_from_
    /// table` would build if this scope had been on disk all along.
    #[test]
    fn overwrite_with_replaces_source_and_fields_together() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        let frame_scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".to_string(),
                values: vec!["BK002".to_string(), "BK003".to_string()],
            }],
            text: Some("spx".to_string()),
            ..Scope::default()
        };
        overwrite_with(&mut draft, &frame_scope, &config);

        let FieldKind::OrderedList { items, .. } = &draft.fields[0].kind else {
            panic!("dimensions is a list");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "book");
        assert_eq!(items[0].note.as_deref(), Some("BK002, BK003"));
        assert_eq!(draft.fields[1].kind, FieldKind::Text("spx".to_string()));

        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("mine", item);
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
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["mine"], frame_scope);
    }

    /// The review's own named collision (Task 5 review round 1, MINOR):
    /// `dimension_note` joins a dimension's values with `", "`, so a
    /// single value that itself contains `", "` paints identically to
    /// two separate values. If dirtiness were judged from the painted
    /// `dimensions` field alone, overwriting `["BK001", "BK002"]` with
    /// the single value `"BK001, BK002"` would look like no change at
    /// all and `o` would silently fail to write. Pins that
    /// `Draft::is_dirty` (and so `apply::commit_edit`) still sees it,
    /// because dirtiness is judged from `source` — the actual object —
    /// not its painted note.
    #[test]
    fn overwrite_with_is_seen_even_when_the_painted_summary_collides() {
        let config =
            config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\", \"BK002\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(!draft.is_dirty(), "a freshly opened draft starts clean");

        let colliding = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".to_string(),
                values: vec!["BK001, BK002".to_string()],
            }],
            ..Scope::default()
        };
        overwrite_with(&mut draft, &colliding, &config);

        // The painted note really does collide — otherwise this test
        // would not be exercising the branch it claims to.
        let FieldKind::OrderedList { items, .. } = &draft.fields[0].kind else {
            panic!("dimensions is a list");
        };
        assert_eq!(
            items[0].note.as_deref(),
            Some("BK001, BK002"),
            "the two scopes must paint identically for this test to mean anything"
        );
        // ...but the draft is still dirty, and still queues a write,
        // because `source` — the actual object — is not the same table.
        assert!(
            draft.is_dirty(),
            "a genuinely different scope must register as dirty even when its summary collides"
        );
        assert!(
            draft
                .writes_by_destination()
                .contains_key(&Destination::Doc)
        );
    }

    /// The shared walk every domain gets for free (`Domain::objects`,
    /// `derive_rows`): a scope's key IS its name, and the provenance
    /// markers work over it exactly as they do over a view's or a
    /// grouping slot's.
    #[test]
    fn domain_scopes_lists_scopes_with_their_owning_layer() {
        let builtin = LayerDoc::builtin(
            "scopes",
            "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n[us]\n[us.dimensions]\nbook = [\"BK002\"]\n",
        )
        .unwrap();
        let user = LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: "<test:user>".into(),
            table: "[eu]\n[eu.dimensions]\nbook = [\"BK099\"]\n"
                .parse()
                .unwrap(),
        };
        let config = Config::load(&ConfigSources {
            builtin: vec![builtin, user],
            desk: None,
            user: None,
        });
        let rows = Domain::Scopes.objects(&config);
        let eu = rows.iter().find(|r| r.name == "eu").expect("eu");
        let us = rows.iter().find(|r| r.name == "us").expect("us");
        assert_eq!(eu.layer, Some(Layer::User));
        assert!(eu.overridden);
        assert_eq!(us.layer, Some(Layer::Builtin));
        assert!(!us.overridden);
    }

    /// The values list: every delivered value with its count, ticked iff
    /// saved; then every saved value the data lacks, ticked and marked.
    #[test]
    fn values_fields_tick_the_saved_ones_and_keep_a_stale_one_marked() {
        let saved = vec!["BK001".to_string(), "BK009".to_string()];
        let fields = values_fields(&saved, &[("BK000".into(), 5), ("BK001".into(), 7)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].key, "values");
        let FieldKind::OrderedList { items, available } = &fields[0].kind else {
            panic!("a list");
        };
        assert!(available.is_none(), "ticking is membership here");
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["BK000", "BK001", "BK009"]);
        assert!(!items[0].included);
        assert_eq!(items[0].note.as_deref(), Some("5"));
        assert!(items[1].included);
        assert!(items[2].included);
        assert_eq!(items[2].note.as_deref(), Some("not in data"));
    }

    /// `fold_values` writes the ticked values under the column; an
    /// emptied selection removes the key (never an empty array) and the
    /// parent's `dimensions` list follows in both directions.
    #[test]
    fn fold_values_writes_the_ticks_and_removes_an_emptied_selection() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values(
            "book",
            values_fields(
                &["BK001".into()],
                &[("BK000".into(), 5), ("BK001".into(), 7)]
            )
        ));
        // Tick BK000 too.
        draft.selected = 1;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        fold_values(&mut draft);
        let dims = draft.source["dimensions"].as_table().unwrap();
        let book: Vec<&str> = dims["book"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(book, ["BK000", "BK001"]);
        // The parent list (stashed) already carries the new note.
        let own = draft.leave_values().unwrap();
        assert_eq!(
            draft.list_items("dimensions").unwrap()[0].note.as_deref(),
            Some("BK000, BK001")
        );

        // Now untick everything: the key goes, the column returns to
        // the available block.
        assert!(draft.enter_values("book", own));
        for row in [1usize, 2] {
            draft.selected = row;
            let _ = draft.toggle_selected();
        }
        fold_values(&mut draft);
        assert!(
            !draft.source["dimensions"]
                .as_table()
                .unwrap()
                .contains_key("book")
        );
        draft.leave_values();
        assert!(draft.list_items("dimensions").unwrap().is_empty());
        assert!(
            draft
                .available_items("dimensions")
                .unwrap()
                .iter()
                .any(|i| i.name == "book")
        );
        assert!(draft.is_dirty());
    }

    /// A first tick on a column that was only available inserts the
    /// selection — and the item — where none existed.
    #[test]
    fn a_first_tick_inserts_a_new_selection() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values("book", values_fields(&[], &[("BK000".into(), 5)])));
        draft.selected = 1;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        fold_values(&mut draft);
        assert_eq!(
            draft.source["dimensions"]["book"].as_array().unwrap()[0].as_str(),
            Some("BK000")
        );
        draft.leave_values();
        assert_eq!(draft.list_items("dimensions").unwrap()[0].name, "book");
        assert!(
            draft
                .available_items("dimensions")
                .unwrap()
                .iter()
                .all(|i| i.name != "book")
        );
    }

    /// The scope the distinct request carries is the DRAFT's, minus the
    /// column being asked about.
    #[test]
    fn draft_scope_is_the_drafts_own_minus_the_column() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\"]\nlhu = [\"L1\"]\n",
        );
        let draft = Domain::Scopes.draft(&config, "mine");
        let scope = draft_scope(&draft, &config, "book");
        assert_eq!(scope.text.as_deref(), Some("spx"));
        assert_eq!(scope.dimensions.len(), 1);
        assert_eq!(scope.dimensions[0].column, "lhu");
    }
}
