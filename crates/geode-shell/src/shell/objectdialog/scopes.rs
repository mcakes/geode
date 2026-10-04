//! Object-dialog adapter for saved scopes.
//!
//! A scope draft owns ordered dimension selections, ticked named-expression
//! references, and optional text and expression filters. The values stage
//! edits one dimension's selected values. Expressions are parsed before
//! persistence, so invalid text is refused with its parser diagnostic
//! instead of being written and dropped on reload.
//! Folding always updates the draft's source table before validation or write.

use geode_core::config::{
    Config, Diagnostic, EXPRESSIONS_DOC, Layer, LayerDoc, Severity, merge_docs,
};
use geode_core::dimensions::DerivedDimensions;
use geode_core::named::{NamedExpr, NamedExpressions};
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::scopes::saved_scopes_from_doc;

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
    let named = named_of(Some(table));
    if !named.is_empty() {
        parts.push(format!("≡ {}", named.join(", ")));
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

/// Build dimension selections, text, and expression fields. Missing objects use empty
/// defaults so the same builder supports naming a new scope.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(config, table)
}

/// The shared body of [`fields`] and [`overwrite_with`]: both need the
/// same four rows built from a raw scope table and the live `Config`
/// (for the two lists' available catalogues), one read off
/// `Config` itself, the other freshly rendered from the frame's own
/// scope.
///
/// `pub(super)` rather than private: [`super::Domain::fields_from_source`] is `c`'s
/// other caller, building the copy's fields straight from the table it just cloned
/// rather than from a named object `config` has a row for yet.
pub(super) fn fields_from_table(config: &Config, table: Option<&toml::Table>) -> Vec<Field> {
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
    let (named_items, named_available) = named_lists(config, table);
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
            key: "named".to_string(),
            label: "Named expressions".to_string(),
            kind: FieldKind::OrderedList {
                items: named_items,
                available: Some(named_available),
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

/// The merged `expressions` doc, read against the current schema's vocabulary.
/// The entry diagnostics are dropped: the loader already logs them, and this
/// dialog reports only the references its own scope makes.
fn named_expressions(config: &Config) -> NamedExpressions {
    let vocab = crate::shell::expr_vocab(config);
    config
        .doc(EXPRESSIONS_DOC)
        .map(|doc| NamedExpressions::from_doc(doc, &vocab).0)
        .unwrap_or_default()
}

/// The names a raw scope table references, in file order.
fn named_of(table: Option<&toml::Table>) -> Vec<String> {
    table
        .and_then(|t| t.get("named"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `named` list: the scope's references ticked, in order, each noted
/// `missing` or `invalid` when the expressions doc cannot supply it, and
/// every other defined name available. A dangling reference stays listed
/// so it can be seen and unticked rather than silently vanishing.
fn named_lists(config: &Config, table: Option<&toml::Table>) -> (Vec<ListItem>, Vec<ListItem>) {
    let defined = named_expressions(config);
    let item = |name: String, included: bool, note: Option<&str>| ListItem {
        name,
        included,
        presentation: Default::default(),
        kind: None,
        note: note.map(str::to_string),
    };
    let items: Vec<ListItem> = named_of(table)
        .into_iter()
        .map(|name| {
            let note = match defined.get(&name) {
                None => Some("missing"),
                Some(NamedExpr::Invalid { .. }) => Some("invalid"),
                Some(NamedExpr::Valid { .. }) => None,
            };
            item(name, true, note)
        })
        .collect();
    let available = defined
        .names()
        .filter(|name| !items.iter().any(|i| i.name == *name))
        .map(|name| item(name.to_string(), false, None))
        .collect();
    (items, available)
}

/// Scope selections have no meaningful order; reorder requests return this notice.
pub const NO_ORDER_NOTICE: &str = "selections have no order";

/// The draft rendered as `scopes.toml`'s own value for this object:
/// `draft.source`, unchanged but for a new object's three keys defaulted
/// in (below), wrapped for [`super::Domain::to_table`]'s sake.
///
/// Never derived from `draft.fields` — both fields are
/// [`FieldKind::Text`], painted summaries with nothing in the field
/// vocabulary that could reconstruct a `dimensions` sub-table or an
/// `expression` string from them. `draft.source` is the actual object,
/// exactly as `Domain::draft` first read it off the merged doc, and the
/// **only** things that ever change it are [`overwrite_with`] and `c`'s
/// own copy (`render::create_from_name`) — the same "the field vocabulary
/// does not model this, so render from `source`" shape `views::doc_table`'s
/// own doc comment describes, taken all the way to its limit: here,
/// nothing but `source` is ever rendered.
///
/// `n`'s freshly named object has an EMPTY `source` — `Draft::new_object`'s own literal
/// — which would otherwise render `{}`, giving the file no hint of the object's shape.
/// Cloning `source` and defaulting the three keys in (never mutating the draft itself,
/// which stays the single "what did the trader actually change" source of truth) is
/// what makes a fresh scope's file entry show an empty `dimensions` table, `text = ""`
/// and `expression = ""` on the very first write — a no-op for any table that already
/// has them, which is every table but a brand new one.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut source = draft.source.clone();
    source
        .entry("dimensions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    source
        .entry("text".to_string())
        .or_insert_with(|| toml::Value::String(String::new()));
    source
        .entry("expression".to_string())
        .or_insert_with(|| toml::Value::String(String::new()));
    toml_edit::Item::Table(super::toml_table_to_edit(&source))
}

/// Validate only this rendered scope through `saved_scopes_from_doc`, avoiding other
/// objects' diagnostics. That reader reports warnings, so these diagnostics do not trip
/// the error-only batch gate. Typed expression entry separately refuses parse failures
/// before changing the field.
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
    let (_saved, mut diags) = saved_scopes_from_doc(&doc, &schema, &dims);
    diags.extend(named_reference_warnings(draft, config));
    diags
}

/// One warning per ticked name the expressions doc cannot supply. A warning,
/// not an error: the reference is legitimate to save (the definition may be
/// written next), and the query reports it when the scope is applied.
fn named_reference_warnings(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let defined = named_expressions(config);
    named_of(Some(&draft.source))
        .into_iter()
        .filter_map(|name| {
            let message = match defined.get(&name) {
                None => format!("named expression '{name}' is missing"),
                Some(NamedExpr::Invalid { reason, .. }) => {
                    format!("named expression '{name}' is invalid: {reason}")
                }
                Some(NamedExpr::Valid { .. }) => return None,
            };
            Some(Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message,
                path: Some(format!("{DOC}.{}.named.{name}", draft.name)),
            })
        })
        .collect()
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
/// `run_confirmed` reads `shell.target_frame().read(cx).scope().clone()` at the
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
    draft.source = super::apply::scope_as_toml(scope);
    draft.fields = fields_from_table(config, Some(&draft.source));
}

/// Normalize text and parse expressions before committing typed entry. Invalid
/// expressions remain in the open field with a refusal instead of reaching a reader
/// that would later drop the scope with a warning.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if key == "expression" && !text.is_empty() {
        geode_core::scope::parse_expr(text).map_err(|e| format!("expression: {e}"))?;
    }
    Ok(text.to_string())
}

/// Fold text/expression fields and retained dimension names into `source` before
/// validation and writing. Keep selected values unchanged here; the Values stage owns
/// them through `fold_values`.
///
/// The first change can add empty text/expression keys to a source that omitted them. A
/// tick followed by an untick can therefore change file text even when the scope's
/// effective selection returns to its starting value.
pub fn fold(draft: &mut Draft) {
    let mut text = None;
    let mut expression = None;
    // `None` while the `dimensions` field is not installed — the Values
    // stage has stashed it — so the fold never retains against an empty
    // list and wipes every selection (the trap CLAUDE.md records).
    let mut kept: Option<Vec<String>> = None;
    // `None` for the same reason: the Values stage stashes this list too.
    let mut named: Option<Vec<String>> = None;
    for field in &draft.fields {
        match (field.key.as_str(), &field.kind) {
            ("text", FieldKind::Text(t)) => text = Some(t.clone()),
            ("expression", FieldKind::Text(e)) => expression = Some(e.clone()),
            ("dimensions", FieldKind::OrderedList { items, .. }) => {
                kept = Some(items.iter().map(|i| i.name.clone()).collect());
            }
            ("named", FieldKind::OrderedList { items, .. }) => {
                named = Some(
                    items
                        .iter()
                        .filter(|i| i.included)
                        .map(|i| i.name.clone())
                        .collect(),
                );
            }
            _ => {}
        }
    }
    // Only ticked names are written; none ticked removes the key, since the
    // reader and `scope_to_table` both treat an absent key as the empty list.
    match named {
        Some(named) if named.is_empty() => {
            draft.source.remove("named");
        }
        Some(named) => {
            draft.source.insert(
                "named".into(),
                toml::Value::Array(named.into_iter().map(toml::Value::String).collect()),
            );
        }
        None => {}
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

/// The Values stage's one field before the data answers: a display-only row, so the
/// stage has a shape to paint and `escape` to leave by. Replaced whole by
/// [`values_fields`] on delivery.
///
/// `render::enter_values_stage` seeds a fresh Values stage with this
/// before the `Request::Distinct` round trip lands.
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

/// Delivered values in outcome order, with counts and the draft's selected ticks.
/// Append selected values missing from the data and mark them `not in data` so they
/// remain visible and removable.
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

/// The Values stage's fold: the ticked values become `source.dimensions.<column>` — the
/// key removed outright when none is ticked, since an empty array is never written —
/// and the stashed parent's `dimensions` list follows: a first tick inserts the item
/// (out of the available block), an emptied selection returns it there, a changed
/// selection refreshes the note. Called from `render::revalidate` on every tick while
/// [`Draft::values`] is `Some`; a no-op while the stage still shows its loading/failed
/// row.
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

/// The draft's scope as `saved_scopes_from_doc` would read it, with `minus`'s own
/// selection removed — what the distinct request carries, so a value's count answers
/// "within the scope I am authoring". An unreadable draft (the reader warns and drops
/// it) yields the empty scope: the counts are then dataset-wide, which is honest for a
/// scope that does not yet parse.
///
/// `render::enter_values_stage` and [`expression_scope`] are the callers.
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

/// The scope a new `expression` for this draft is ANDed with: its own
/// selections and text filter, with no expression, since that is being
/// replaced. Narrows the expression field's value suggestions.
///
/// The old expression is blanked before the scope is read, not after: the
/// reader drops a whole scope whose expression it cannot read (one naming
/// a removed column), which would widen the request to the whole dataset
/// exactly when the field is opened to repair it. `to_table` renders from
/// `source` alone, so that is the only place it is blanked.
pub fn expression_scope(draft: &Draft, config: &Config) -> Scope {
    let mut blanked = draft.clone();
    blanked.source.remove("expression");
    let mut scope = draft_scope(&blanked, config, "");
    scope.expression = None;
    scope
}

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)).
pub fn help(key: &str) -> &'static str {
    match key {
        "dimensions" => {
            "The dimensions this scope narrows — open one to tick its values, x drops it"
        }
        // One footer line holds at most 90 characters (`help_fits`).
        "named" => {
            "Named expressions ANDed with this scope; editing one changes every scope that ticks it"
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

    /// Four fields: the selected dimensions as a list with the other
    /// pickable columns available, the named-expression list, then the
    /// text filter and the expression as editable text.
    #[test]
    fn fields_are_the_dimensions_list_the_text_filter_and_the_expression() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\nexpression = \"npv > 0\"\n[mine.dimensions]\nbook = [\"BK001\", \"BK003\"]\n\n[bare]\n",
        );
        let fields = Domain::Scopes.fields(&config, Some("mine"));
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].key, "dimensions");
        assert_eq!(fields[1].key, "named");
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
        assert_eq!(fields[2].key, "text");
        assert_eq!(fields[2].kind, FieldKind::Text("spx".to_string()));
        assert_eq!(fields[3].key, "expression");
        assert_eq!(fields[3].kind, FieldKind::Text("npv > 0".to_string()));

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
        assert_eq!(bare[2].kind, FieldKind::Text(String::new()));
        assert_eq!(bare[3].kind, FieldKind::Text(String::new()));
    }

    #[test]
    fn an_object_that_does_not_exist_has_the_empty_fields_rather_than_panicking() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Scopes.fields(&config, object);
            assert!(
                matches!(&fields[0].kind, FieldKind::OrderedList { items, .. } if items.is_empty())
            );
            assert_eq!(fields[2].kind, FieldKind::Text(String::new()));
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
        draft.fields[2].kind = FieldKind::Text("ndx".to_string());
        draft.fields[3].kind = FieldKind::Text("npv > 0".to_string());
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

    /// `n`'s brand new object: `Domain::new_draft`'s `source` is `toml::Table::new()`,
    /// empty — `to_table` still renders the three keys a saved scope always has, so the
    /// very first write shows the object's shape rather than a bare `{}`.
    #[test]
    fn to_table_defaults_the_three_keys_for_a_brand_new_object() {
        let config = config_with_scope("");
        let draft = Domain::Scopes.new_draft(&config, "today");
        assert!(draft.source.is_empty(), "a fresh draft's source is empty");
        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("today", item);
        // `toml_edit` renders an empty table as its own `[today.
        // dimensions]` header rather than an inline `{}` — this is the
        // shape a real write produces, so the test reads that shape
        // rather than the doc comment's inline shorthand.
        assert!(text.contains("[today.dimensions]"), "{text}");
        assert!(text.contains("text = \"\""), "{text}");
        assert!(text.contains("expression = \"\""), "{text}");
        // The draft itself is untouched — `to_table` renders a clone.
        assert!(draft.source.is_empty());
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
        assert_eq!(draft.fields[2].kind, FieldKind::Text("spx".to_string()));

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

    /// Different scope selections can have the same truncated summary. Dirtiness must
    /// compare source values as well as fields so an overwrite still persists.
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

    /// The saved expression is the one being replaced, so an unreadable one
    /// (a removed column) must not drop the draft's selections from the
    /// values request's narrowing — that is when the field is opened to
    /// repair it.
    #[test]
    fn expression_scope_keeps_the_selections_under_an_unreadable_expression() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\nexpression = \"gone = 'x'\"\n\
             [mine.dimensions]\nbook = [\"BK001\"]\n",
        );
        let draft = Domain::Scopes.draft(&config, "mine");
        let scope = expression_scope(&draft, &config);
        assert_eq!(scope.dimensions.len(), 1);
        assert_eq!(scope.dimensions[0].column, "book");
        assert_eq!(scope.dimensions[0].values, ["BK001"]);
        assert_eq!(scope.text.as_deref(), Some("spx"));
        assert!(scope.expression.is_none());
    }

    fn config_with_scope_and_expressions(scopes: &str, expressions: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\ncategorical = true\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let scopes = LayerDoc::builtin("scopes", scopes).unwrap();
        let expressions =
            LayerDoc::builtin(geode_core::config::EXPRESSIONS_DOC, expressions).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, scopes, expressions],
            desk: None,
            user: None,
        })
    }

    fn list_rows(list: &[ListItem]) -> Vec<(&str, bool, Option<&str>)> {
        list.iter()
            .map(|i| (i.name.as_str(), i.included, i.note.as_deref()))
            .collect()
    }

    /// The named list holds the scope's own references in order, each
    /// ticked, a dangling one noted `missing` and an unparseable one
    /// `invalid`; every other defined name is available, unticked.
    #[test]
    fn the_named_field_notes_missing_and_invalid_names_and_offers_the_rest() {
        let config = config_with_scope_and_expressions(
            "[mine]\nnamed = [\"liq\", \"gone\", \"broken\"]\n",
            "[liq]\nexpression = \"npv > 0\"\n[broken]\nexpression = \"npv >\"\n\
             [hedges]\nexpression = \"npv < 0\"\n",
        );
        let fields = Domain::Scopes.fields(&config, Some("mine"));
        assert_eq!(fields[0].key, "dimensions");
        assert_eq!(fields[1].key, "named");
        assert_eq!(fields[1].label, "Named expressions");
        let FieldKind::OrderedList { items, available } = &fields[1].kind else {
            panic!("named is a list");
        };
        assert_eq!(
            list_rows(items),
            [
                ("liq", true, None),
                ("gone", true, Some("missing")),
                ("broken", true, Some("invalid")),
            ]
        );
        assert_eq!(
            list_rows(available.as_deref().unwrap()),
            [("hedges", false, None)]
        );
    }

    /// `fold` writes the ticked names, in list order, and drops the key
    /// outright once none is ticked.
    #[test]
    fn fold_writes_only_ticked_names_and_drops_the_key_when_none_is() {
        let config = config_with_scope_and_expressions(
            "[mine]\nnamed = [\"liq\", \"hedges\"]\n",
            "[liq]\nexpression = \"npv > 0\"\n[hedges]\nexpression = \"npv < 0\"\n",
        );
        let mut draft = Domain::Scopes.draft(&config, "mine");
        let set_ticks = |draft: &mut Draft, liq: bool, hedges: bool| {
            let FieldKind::OrderedList { items, .. } = &mut draft.fields[1].kind else {
                panic!("named is a list");
            };
            items[0].included = liq;
            items[1].included = hedges;
        };
        set_ticks(&mut draft, false, true);
        fold(&mut draft);
        let written: Vec<&str> = draft.source["named"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(written, ["hedges"]);
        set_ticks(&mut draft, false, false);
        fold(&mut draft);
        assert!(!draft.source.contains_key("named"));
    }

    /// While the Values stage has stashed the object's fields, `fold`
    /// leaves `source.named` alone rather than dropping it.
    #[test]
    fn fold_leaves_named_alone_while_the_values_stage_is_open() {
        let config = config_with_scope_and_expressions(
            "[mine]\nnamed = [\"liq\"]\n[mine.dimensions]\nbook = [\"BK001\"]\n",
            "[liq]\nexpression = \"npv > 0\"\n",
        );
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values("book", Vec::new()));
        fold(&mut draft);
        assert!(draft.source.contains_key("named"));
    }

    /// The browse summary names the scope's named expressions after its
    /// selections.
    #[test]
    fn the_summary_names_the_named_expressions() {
        assert_eq!(
            summary(&value(
                "named = [\"liq\", \"hedges\"]\n[dimensions]\nbook = [\"BK001\"]\n"
            )),
            "book ∈ BK001; ≡ liq, hedges"
        );
    }

    /// A ticked name that is missing or invalid warns at its own path and
    /// flags its own row; neither blocks the commit.
    #[test]
    fn validate_warns_for_a_missing_or_invalid_ticked_name() {
        let config = config_with_scope_and_expressions(
            "[mine]\nnamed = [\"liq\", \"gone\", \"broken\"]\n",
            "[liq]\nexpression = \"npv > 0\"\n[broken]\nexpression = \"npv >\"\n",
        );
        let mut draft = Domain::Scopes.draft(&config, "mine");
        fold(&mut draft);
        let diags = validate(&draft, &config);
        let named: Vec<(&str, &str)> = diags
            .iter()
            .filter(|d| {
                d.path
                    .as_deref()
                    .is_some_and(|p| p.starts_with("scopes.mine.named."))
            })
            .map(|d| (d.path.as_deref().unwrap(), d.message.as_str()))
            .collect();
        assert_eq!(named.len(), 2, "{diags:?}");
        assert_eq!(
            named[0],
            (
                "scopes.mine.named.gone",
                "named expression 'gone' is missing"
            )
        );
        assert_eq!(named[1].0, "scopes.mine.named.broken");
        assert!(
            named[1]
                .1
                .starts_with("named expression 'broken' is invalid: "),
            "{}",
            named[1].1
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity == geode_core::config::Severity::Warning)
        );
        assert_eq!(
            draft.row_for_path(DOC, "scopes.mine.named.gone"),
            Some(super::super::EditRow::Item { field: 1, item: 1 })
        );
    }
}
