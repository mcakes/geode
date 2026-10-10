//! Watchlists: named lists of underlyings (`watchlists.toml`). A list holds
//! manual names (`include`), names it drops (`exclude`), and rules that
//! derive names from the `underlying_ref` values a dataset holds under a
//! scope. This module is pure: the reader, the name rules, the editing
//! verbs, the TOML the config door writes, and the resolved snapshot a
//! consumer reads. Resolution itself runs in the data layer.

pub mod edit;
pub mod fold;
pub mod members;
pub mod state;

pub use crate::config::WATCHLISTS_DOC;
pub use crate::link::UNDERLYING;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::scope::expr::KEYWORDS;
use std::collections::BTreeMap;

/// One rule: a dataset and at most one of a saved scope name or an inline
/// expression. Kept as text so the object round-trips; `fold::fold_rules`
/// validates and resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rule {
    pub dataset: String,
    pub scope: Option<String>,
    pub expression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Watchlist {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Watchlists {
    map: BTreeMap<String, Watchlist>,
}

impl Watchlists {
    pub fn get(&self, name: &str) -> Option<&Watchlist> {
        self.map.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Watchlist)> {
        self.map.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn insert(&mut self, name: String, list: Watchlist) {
        self.map.insert(name, list);
    }

    pub fn remove(&mut self, name: &str) -> Option<Watchlist> {
        self.map.remove(name)
    }
}

fn warn(path: String, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message,
        path: Some(path),
    }
}

fn error(path: String, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: Some(path),
    }
}

/// An array of names: trimmed, blanks dropped, repeats kept once in first
/// order. A non-array warns and reads as empty; a non-string entry warns
/// and is dropped.
pub fn names_list(
    value: Option<&toml::Value>,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(value) = value else { return out };
    let Some(items) = value.as_array() else {
        diags.push(warn(
            path.to_string(),
            format!("{path} must be an array of names; ignored"),
        ));
        return out;
    };
    for (i, item) in items.iter().enumerate() {
        match item.as_str() {
            Some(s) => {
                let s = s.trim();
                if !s.is_empty() && !out.iter().any(|n| n == s) {
                    out.push(s.to_string());
                }
            }
            None => diags.push(warn(
                format!("{path}[{i}]"),
                format!("{path}[{i}] is not a string; dropped"),
            )),
        }
    }
    out
}

/// A rule table's string key, trimmed. A non-string value warns and reads
/// as absent.
fn rule_text(
    table: &toml::Table,
    path: &str,
    key: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<String> {
    let value = table.get(key)?;
    match value.as_str() {
        Some(s) => Some(s.trim().to_string()),
        None => {
            diags.push(warn(
                format!("{path}.{key}"),
                format!("{path}.{key} must be a string; ignored"),
            ));
            None
        }
    }
}

fn read_rule(value: &toml::Value, path: &str, diags: &mut Vec<Diagnostic>) -> Rule {
    let Some(table) = value.as_table() else {
        diags.push(warn(
            path.to_string(),
            format!("{path} must be a table; kept as an empty rule"),
        ));
        return Rule::default();
    };
    let dataset = rule_text(table, path, "dataset", diags).unwrap_or_default();
    let scope = rule_text(table, path, "scope", diags).filter(|s| !s.is_empty());
    let expression = rule_text(table, path, "expression", diags).filter(|s| !s.is_empty());
    if dataset.is_empty() {
        diags.push(warn(path.to_string(), format!("{path} names no dataset")));
    }
    Rule {
        dataset,
        scope,
        expression,
    }
}

/// Read the merged document. A list is dropped only when its object is not
/// a table, a name is in both `include` and `exclude`, or its name clashes
/// (ignoring Unicode case) with an earlier list; every other problem warns
/// and keeps the list. Rules are kept whatever their shape so the fold can
/// report them by index.
pub fn from_doc(doc: &MergedDoc) -> (Watchlists, Vec<Diagnostic>) {
    let mut out = Watchlists::default();
    let mut diags = Vec::new();
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        let path = format!("watchlists.{name}");
        let Some(table) = value.as_table() else {
            diags.push(warn(
                path,
                format!("watchlists: '{name}' must be a table; ignored"),
            ));
            continue;
        };
        if let Err(reason) = validate_name(name, &out) {
            diags.push(error(
                path,
                format!("watchlists: {reason}; '{name}' dropped"),
            ));
            continue;
        }
        let include = names_list(table.get("include"), &format!("{path}.include"), &mut diags);
        let exclude = names_list(table.get("exclude"), &format!("{path}.exclude"), &mut diags);
        if let Some(both) = include.iter().find(|n| exclude.contains(n)) {
            diags.push(error(
                path,
                format!("watchlists: '{name}' lists '{both}' in both include and exclude; dropped"),
            ));
            continue;
        }
        let rules = match table.get("rules") {
            None => Vec::new(),
            Some(v) => match v.as_array() {
                Some(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, r)| read_rule(r, &format!("{path}.rules[{i}]"), &mut diags))
                    .collect(),
                None => {
                    diags.push(warn(
                        format!("{path}.rules"),
                        format!("{path}.rules must be an array of tables; ignored"),
                    ));
                    Vec::new()
                }
            },
        };
        out.insert(
            name.clone(),
            Watchlist {
                include,
                exclude,
                rules,
            },
        );
    }
    (out, diags)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A new, cloned or renamed list's name: an identifier, not a scope
/// keyword, not the version stamp, and not an existing list ignoring case.
pub fn validate_name(name: &str, existing: &Watchlists) -> Result<(), String> {
    if !is_identifier(name) {
        return Err(format!(
            "'{name}' is not a valid name: use letters, digits and _, not starting with a digit"
        ));
    }
    if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        return Err(format!("'{name}' is a reserved word in scope expressions"));
    }
    if name.eq_ignore_ascii_case("config_version") {
        return Err(format!("'{name}' is reserved ('config_version')"));
    }
    if let Some(other) = existing
        .names()
        .find(|n| n.to_lowercase() == name.to_lowercase())
    {
        return Err(format!("'{name}' already exists ('{other}')"));
    }
    Ok(())
}

/// The object the config door writes: `include`, `exclude` (omitted when
/// empty), then `rules` (omitted when empty), each rule `dataset` then its
/// `scope` or `expression`.
pub fn to_toml(list: &Watchlist) -> toml::Value {
    let strings =
        |v: &[String]| toml::Value::Array(v.iter().cloned().map(toml::Value::String).collect());
    let mut table = toml::Table::new();
    table.insert("include".into(), strings(&list.include));
    if !list.exclude.is_empty() {
        table.insert("exclude".into(), strings(&list.exclude));
    }
    if !list.rules.is_empty() {
        let rules = list
            .rules
            .iter()
            .map(|r| {
                let mut t = toml::Table::new();
                t.insert("dataset".into(), toml::Value::String(r.dataset.clone()));
                if let Some(s) = &r.scope {
                    t.insert("scope".into(), toml::Value::String(s.clone()));
                }
                if let Some(e) = &r.expression {
                    t.insert("expression".into(), toml::Value::String(e.clone()));
                }
                toml::Value::Table(t)
            })
            .collect();
        table.insert("rules".into(), toml::Value::Array(rules));
    }
    toml::Value::Table(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> MergedDoc {
        merge_docs(
            "watchlists",
            &[LayerDoc::builtin("watchlists", text).unwrap()],
        )
    }

    #[test]
    fn a_list_reads_its_names_and_rules_in_order() {
        let (lists, diags) = from_doc(&doc(
            "[eu]\ninclude = [\"SX5E\", \"DAX\"]\nexclude = [\"SMI\"]\n\
             [[eu.rules]]\ndataset = \"risk_snapshot\"\nscope = \"europe\"\n\
             [[eu.rules]]\ndataset = \"cvi_params\"\nexpression = \"region = 'Europe'\"\n",
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let eu = lists.get("eu").unwrap();
        assert_eq!(eu.include, vec!["SX5E", "DAX"]);
        assert_eq!(eu.exclude, vec!["SMI"]);
        assert_eq!(eu.rules.len(), 2);
        assert_eq!(
            eu.rules[0],
            Rule {
                dataset: "risk_snapshot".into(),
                scope: Some("europe".into()),
                expression: None
            }
        );
        assert_eq!(eu.rules[1].expression.as_deref(), Some("region = 'Europe'"));
    }

    #[test]
    fn include_entries_are_trimmed_deduplicated_and_blanks_dropped() {
        let (lists, diags) = from_doc(&doc(
            "[a]\ninclude = [\" SPX \", \"SPX\", \"\", 3, \"NDX\"]\n",
        ));
        assert_eq!(lists.get("a").unwrap().include, vec!["SPX", "NDX"]);
        // The integer entry warns; nothing else does.
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("not a string"));
    }

    #[test]
    fn a_name_in_both_include_and_exclude_drops_the_object_with_an_error() {
        let (lists, diags) = from_doc(&doc(
            "[a]\ninclude = [\"SPX\"]\nexclude = [\"SPX\"]\n[b]\ninclude = [\"NDX\"]\n",
        ));
        assert!(lists.get("a").is_none());
        assert!(lists.get("b").is_some());
        assert!(diags.iter().any(|d| d.severity == Severity::Error
            && d.message.contains("SPX")
            && d.path.as_deref() == Some("watchlists.a")));
    }

    #[test]
    fn a_case_insensitive_clash_keeps_the_first_and_drops_the_second() {
        let (lists, diags) = from_doc(&doc(
            "[eu]\ninclude = [\"SPX\"]\n[EU]\ninclude = [\"NDX\"]\n",
        ));
        assert_eq!(lists.names().collect::<Vec<_>>(), vec!["eu"]);
        assert!(diags.iter().any(|d| d.severity == Severity::Error
            && d.message.contains("'EU'")
            && d.message.contains("'eu'")));
    }

    #[test]
    fn a_non_table_object_and_a_bad_rule_shape_warn() {
        let (lists, diags) = from_doc(&doc("a = 1\n[b]\nrules = [{ scope = \"x\" }]\n"));
        assert!(lists.get("a").is_none());
        // The rule without a dataset is kept with an empty dataset so the
        // fold reports it by index; the reader only warns.
        assert_eq!(lists.get("b").unwrap().rules.len(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("'a'") && d.message.contains("table"))
        );
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("watchlists.b.rules[0]")
                    && d.message.contains("dataset"))
        );
    }

    #[test]
    fn validate_name_refuses_keywords_version_and_clashes() {
        let (lists, _) = from_doc(&doc("[eu]\n"));
        assert!(validate_name("eu", &lists).is_err());
        assert!(validate_name("Eu", &lists).unwrap_err().contains("'eu'"));
        assert!(validate_name("not", &lists).is_err());
        assert!(validate_name("config_version", &lists).is_err());
        assert!(validate_name("eu core", &lists).is_err());
        assert!(validate_name("", &lists).is_err());
        assert!(validate_name("us_core", &lists).is_ok());
    }

    #[test]
    fn to_toml_round_trips_and_omits_empty_exclude() {
        let text = "[eu]\ninclude = [\"SX5E\"]\n[[eu.rules]]\ndataset = \"risk_snapshot\"\nexpression = \"book = 'BK000'\"\n";
        let (lists, _) = from_doc(&doc(text));
        let value = to_toml(lists.get("eu").unwrap());
        let table = value.as_table().unwrap();
        assert_eq!(table.keys().collect::<Vec<_>>(), vec!["include", "rules"]);
        let rule = table["rules"].as_array().unwrap()[0].as_table().unwrap();
        assert_eq!(
            rule.keys().collect::<Vec<_>>(),
            vec!["dataset", "expression"]
        );
        // Reading the written object back gives the same list.
        let mut root = toml::Table::new();
        root.insert("eu".into(), value);
        let again = from_doc(&merge_docs(
            "watchlists",
            &[LayerDoc::builtin("watchlists", &root.to_string()).unwrap()],
        ))
        .0;
        assert_eq!(again.get("eu"), lists.get("eu"));
    }
}
