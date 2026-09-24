//! Typed `egress.toml` configuration, with one top-level table per egress
//! target, keyed by name. Layer merging replaces a target's whole table by
//! name, matching `sources.toml`. Parsing returns usable targets and
//! field-addressed diagnostics; an invalid target is skipped entirely.
//!
//! `egress.toml` is restart-required like `sources.toml`: nothing reloads a
//! resolved target's transport live, so a hot reload of this document keeps
//! the last valid set without applying it. This reader performs no I/O and
//! depends only on shared configuration and schema types, mirroring
//! `source_config`. See `docs/current/configuration.md`.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::SchemaSpec;
use toml::Table;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressSpec {
    pub name: String,
    pub adapter: String,
    /// Document dataset name → address template (may contain `{key}`), in
    /// TOML order.
    pub documents: Vec<(String, String)>,
}

impl EgressSpec {
    /// The address for one document key: `{key}` replaced by the parts
    /// joined with `/`. `None` if `document` names no target of this egress.
    pub fn address(&self, document: &str, key: &[String]) -> Option<String> {
        self.documents
            .iter()
            .find(|(name, _)| name == document)
            .map(|(_, template)| template.replace("{key}", &key.join("/")))
    }
}

/// Address diagnostics as `egress.<name>[.<key>]`. Use the deepest known
/// field, or only the target name when the entry is not a table.
fn diag(
    severity: Severity,
    name: &str,
    key: Option<&str>,
    m: impl std::fmt::Display,
) -> Diagnostic {
    Diagnostic {
        severity,
        layer: None,
        file: None,
        message: format!("egress '{name}': {m}"),
        path: Some(match key {
            Some(k) => format!("egress.{name}.{k}"),
            None => format!("egress.{name}"),
        }),
    }
}

/// Validate the `documents` table of one target: every key must name a
/// document-family dataset and hold a string address template. The first
/// invalid entry aborts the whole target, mirroring how `source_config`
/// aborts a source on its first invalid required field.
fn read_documents(
    name: &str,
    table: &Table,
    schema: &SchemaSpec,
) -> Result<Vec<(String, String)>, Diagnostic> {
    let mut documents = Vec::new();
    for (doc_name, value) in table {
        if !schema.dataset(doc_name).is_some_and(|d| d.is_document()) {
            return Err(diag(
                Severity::Error,
                name,
                Some(doc_name),
                format!("'{doc_name}' names no document dataset"),
            ));
        }
        let Some(address) = value.as_str() else {
            return Err(diag(
                Severity::Error,
                name,
                Some(doc_name),
                format!("address for '{doc_name}' must be a string"),
            ));
        };
        documents.push((doc_name.clone(), address.to_string()));
    }
    Ok(documents)
}

pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<EgressSpec>, Vec<Diagnostic>) {
    let mut out = Vec::new();
    let mut diags = Vec::new();

    for (name, value) in &doc.value {
        let Some(table) = value.as_table() else {
            diags.push(diag(Severity::Warning, name, None, "not a table"));
            continue;
        };

        let adapter = match table.get("adapter").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("adapter"),
                    "missing 'adapter'",
                ));
                continue;
            }
        };

        let documents_value = table.get("documents");
        let documents_table = match documents_value {
            None => {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("documents"),
                    "missing 'documents'",
                ));
                continue;
            }
            Some(v) => match v.as_table() {
                None => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("documents"),
                        "'documents' must be a table",
                    ));
                    continue;
                }
                Some(t) if t.is_empty() => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("documents"),
                        "'documents' is empty",
                    ));
                    continue;
                }
                Some(t) => t,
            },
        };

        let documents = match read_documents(name, documents_table, schema) {
            Ok(d) => d,
            Err(d) => {
                diags.push(d);
                continue;
            }
        };

        out.push(EgressSpec {
            name: name.clone(),
            adapter,
            documents,
        });
    }

    (out, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, Severity, merge_docs};
    use crate::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        // Two document datasets (egress targets) and one measure dataset, to
        // exercise both the "document family" lookup and its rejection.
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"

[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"

[dividend_schedule]
family = "document"
key = ["underlying_ref"]
axes = ["ex_date"]
[dividend_schedule.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[dividend_schedule.columns.ex_date]
type = "date"
role = "axis"
[dividend_schedule.columns.amount]
type = "f64"
role = "value"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn from(text: &str) -> (Vec<EgressSpec>, Vec<Diagnostic>) {
        let doc = merge_docs("egress", &[LayerDoc::builtin("egress", text).unwrap()]);
        from_doc(&doc, &schema())
    }

    #[test]
    fn missing_adapter_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis.documents]
cvi_params = "marketdata/cvi"
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.adapter"))
            .expect("an error diagnostic on adapter");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("sophis"), "{}", d.message);
    }

    #[test]
    fn a_non_string_adapter_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = 5
[sophis.documents]
cvi_params = "marketdata/cvi"
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.adapter"))
            .expect("an error diagnostic on adapter");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn missing_documents_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.documents"))
            .expect("an error diagnostic on documents");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn a_non_table_documents_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
documents = ["cvi_params"]
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.documents"))
            .expect("an error diagnostic on documents");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn an_empty_documents_table_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.documents"))
            .expect("an error diagnostic on documents");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("empty"), "{}", d.message);
    }

    #[test]
    fn a_documents_key_naming_no_document_dataset_is_an_error_and_the_target_is_dropped() {
        // 'risk_snapshot' exists but is a measure dataset, not a document one.
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
risk_snapshot = "marketdata/risk"
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.risk_snapshot"))
            .expect("an error diagnostic on the offending document key");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("risk_snapshot"), "{}", d.message);

        // An undeclared name is refused the same way.
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
nonesuch = "marketdata/x"
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("egress.sophis.nonesuch")
                    && d.severity == Severity::Error)
        );
    }

    #[test]
    fn a_non_string_address_is_an_error_and_the_target_is_dropped() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
cvi_params = 5
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("egress.sophis.cvi_params"))
            .expect("an error diagnostic on the address");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn a_valid_doc_with_two_targets_preserves_order() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
cvi_params = "marketdata/cvi/{key}"
dividend_schedule = "marketdata/dividend/{key}"

[bbg]
adapter = "bbg_egress"
[bbg.documents]
dividend_schedule = "bbg/dividends"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "sophis");
        assert_eq!(specs[0].adapter, "demo_bus");
        assert_eq!(
            specs[0].documents,
            vec![
                ("cvi_params".to_string(), "marketdata/cvi/{key}".to_string()),
                (
                    "dividend_schedule".to_string(),
                    "marketdata/dividend/{key}".to_string()
                ),
            ],
            "documents keep TOML order"
        );
        assert_eq!(specs[1].name, "bbg");
        assert_eq!(specs[1].adapter, "bbg_egress");
    }

    #[test]
    fn address_substitutes_key_when_present_and_is_unchanged_without_it() {
        let (specs, diags) = from(
            r#"
[sophis]
adapter = "demo_bus"
[sophis.documents]
cvi_params = "marketdata/cvi/{key}"
dividend_schedule = "bbg/dividends"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let spec = &specs[0];
        assert_eq!(
            spec.address("cvi_params", &["SPX".to_string(), "20260101".to_string()]),
            Some("marketdata/cvi/SPX/20260101".to_string()),
            "a two-part key joins with '/'"
        );
        assert_eq!(
            spec.address("dividend_schedule", &["SPX".to_string()]),
            Some("bbg/dividends".to_string()),
            "an address without {{key}} is unchanged"
        );
        assert_eq!(
            spec.address("nonesuch", &["SPX".to_string()]),
            None,
            "an unknown document names no target"
        );
    }

    #[test]
    fn a_non_table_entry_is_skipped_with_a_warning() {
        let (specs, diags) = from("stray = 3\n");
        assert!(specs.is_empty());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
    }
}
