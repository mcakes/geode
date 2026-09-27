//! Adapter for named scope expressions: one expression text saved under a
//! name, which saved scopes and the frame tick instead of copying it.
//!
//! One field, `expression`, written over the object's source so any
//! unmodelled keys survive. A draft whose expression does not parse is an
//! error that blocks the write: a saved invalid definition would reach
//! every scope that ticks it as "invalid". An unknown column only warns,
//! as it does for a scope's own expression; typed entry refuses it
//! separately. Names follow the saved-scope rules.

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, Severity, merge_docs};
use geode_core::named::{NamedExpr, NamedExpressions};

use super::{Destination, Draft, Field, FieldKind};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = geode_core::config::EXPRESSIONS_DOC;

/// The browse row's second line: the expression text on one line, read
/// off the raw table so a definition that does not parse still shows
/// what it says.
pub fn summary(value: &toml::Value) -> String {
    match value
        .as_table()
        .and_then(|t| t.get("expression"))
        .and_then(|v| v.as_str())
    {
        Some(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        None => "no expression".to_string(),
    }
}

/// The one field of `object`, or of a new expression when `object`
/// names nothing.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(table)
}

/// The field built from a raw expression table. `c` uses it directly on
/// the copied table, which has no object in `config` yet.
pub(super) fn fields_from_table(table: Option<&toml::Table>) -> Vec<Field> {
    let text = table
        .and_then(|t| t.get("expression"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    vec![Field {
        key: "expression".to_string(),
        label: "Expression".to_string(),
        kind: FieldKind::Text(text.to_string()),
        dest: Destination::Doc,
        layer: None,
    }]
}

/// The draft's source with `expression` set from the field.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut table = super::toml_table_to_edit(&draft.source);
    let text = draft.fields.iter().find_map(|f| match (&f.key, &f.kind) {
        (key, FieldKind::Text(text)) if key == "expression" => Some(text.as_str()),
        _ => None,
    });
    table["expression"] = toml_edit::value(text.unwrap_or_default());
    toml_edit::Item::Table(table)
}

/// The rendered object read back through `NamedExpressions::from_doc`,
/// alone in a doc of its own so other definitions' problems are not
/// reported against this one. The reader only warns; an entry it keeps as
/// `Invalid` is raised to an error here so the write is refused.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table: rendered_doc_table(draft),
        }],
    );
    let vocab = crate::shell::expr_vocab(config);
    let (named, mut diags) = NamedExpressions::from_doc(&doc, &vocab);
    if matches!(named.get(&draft.name), Some(NamedExpr::Invalid { .. })) {
        for d in &mut diags {
            d.severity = Severity::Error;
        }
    }
    diags
}

/// The draft's `expressions.toml` entry, rendered and parsed back the way
/// the loader would read it off disk.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

/// What the field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)). One footer line holds at most
/// 90 characters.
pub fn help(key: &str) -> &'static str {
    match key {
        "expression" => {
            "The expression this name stands for; scopes that tick it AND it in — tab completes"
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use geode_core::config::ConfigSources;

    fn config(expressions: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let expressions = LayerDoc::builtin(DOC, expressions).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, expressions],
            desk: None,
            user: None,
        })
    }

    fn set_expression(draft: &mut Draft, text: &str) {
        draft.fields[0].kind = FieldKind::Text(text.to_string());
    }

    #[test]
    fn an_invalid_expression_is_an_error_and_an_unknown_column_a_warning() {
        let config = config("[liq]\nexpression = \"npv > 0\"\n");
        let mut draft = Domain::Expressions.draft(&config, "liq");
        assert!(draft.diagnostics.is_empty(), "{:?}", draft.diagnostics);
        set_expression(&mut draft, "npv >");
        let diags = validate(&draft, &config);
        assert!(!diags.is_empty());
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        set_expression(&mut draft, "bokk = 'A'");
        let diags = validate(&draft, &config);
        assert!(!diags.is_empty());
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    #[test]
    fn to_table_keeps_unmodelled_keys() {
        let config = config("[liq]\nexpression = \"npv > 0\"\nnote = \"desk\"\n");
        let mut draft = Domain::Expressions.draft(&config, "liq");
        set_expression(&mut draft, "npv < 0");
        let text = super::super::object_text("liq", to_table(&draft, Destination::Doc));
        assert!(text.contains("expression = \"npv < 0\""), "{text}");
        assert!(text.contains("note = \"desk\""), "{text}");
    }

    #[test]
    fn the_summary_is_the_expression_on_one_line() {
        let value: toml::Value = toml::Value::Table(
            "expression = \"npv > 0\\n  and book = 'A'\""
                .parse()
                .unwrap(),
        );
        assert_eq!(summary(&value), "npv > 0 and book = 'A'");
        assert_eq!(
            summary(&toml::Value::Table(toml::Table::new())),
            "no expression"
        );
    }
}
