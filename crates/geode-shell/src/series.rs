//! `[timeseries] default_source` (timeseries spec §9.12) and the fetch
//! sources a tile may name, published as the workspace's THIRD gpui
//! global — admitted under CLAUDE.md's exception for the same reason
//! `linenumbers::UiSettings` was: a module has no path to `ShellView`,
//! the settings row must reach an open tile, and neither
//! `ConfigReloaded` (views and dimensions only) nor the factory (create
//! time only) can carry it. Written by the shell alone: startup, the
//! settings row, hot reload.

use std::path::Path;

use geode_core::config::{Config, Diagnostic, Layer, Severity};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{SourceShape, SourceSpec};
use toml_edit::{Item, Table, value};

/// One configured fetch source and the series dataset it fills — the
/// pair a tile needs to turn `SPX.close@demo_kdb` into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSource {
    pub name: String,
    pub dataset: String,
}

/// The timeseries settings a module may read without a path to
/// `ShellView` — see the module doc. Set by the shell only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesSettings {
    /// `[timeseries] default_source`: the source `:add SPX.close` means
    /// when the identity carries no `@source`. `None` (absent, or naming
    /// nothing configured — see [`default_source_diagnostic`]) means a
    /// tile must be told one explicitly.
    pub default_source: Option<String>,
    /// Every configured fetch source, in `sources` doc order — config
    /// truth, not engine truth: one the engine could not start answers
    /// `Err` on fetch and the slot paints `Failed`.
    pub sources: Vec<FetchSource>,
}

impl gpui::Global for SeriesSettings {}

impl SeriesSettings {
    /// Resolve both halves from the layered config: the fetch sources
    /// from `sources` × `datasets` (`SourceSpec::shape`, the one door
    /// that tells a fetch source from a subscribed or directory one),
    /// the default from `app`. Lenient throughout — a `default_source`
    /// that is not a string, or names nothing, reads as `None` here and
    /// is diagnosed by [`default_source_diagnostic`].
    pub fn from_config(config: &Config) -> SeriesSettings {
        let schema = config
            .doc("datasets")
            .map(|doc| SchemaSpec::from_doc(doc).0)
            .unwrap_or_default();
        let sources = config
            .doc("sources")
            .map(|doc| SourceSpec::from_doc(doc, &schema).0)
            .unwrap_or_default()
            .into_iter()
            .filter(|source| source.shape(&schema) == SourceShape::Fetch)
            .map(|source| FetchSource {
                name: source.name,
                dataset: source.dataset,
            })
            .collect();
        let default_source = config
            .get("app", "timeseries.default_source")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        SeriesSettings {
            default_source,
            sources,
        }
    }

    /// The series dataset `source` fills, or `None` when it is not a
    /// configured fetch source at all — the membership test the
    /// diagnostic and every tile's `@source` resolution share.
    pub fn dataset_of(&self, source: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|s| s.name == source)
            .map(|s| s.dataset.as_str())
    }

    /// The fetch source names in doc order — the settings row's values
    /// and the picker's vocabulary.
    pub fn names(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.name.clone()).collect()
    }
}

/// A warning when the key is not a string or names no fetch source —
/// pure over `&Config`, folded into `ShellView::new`'s startup
/// diagnostics AND `apply_reload`'s, like `modules_default_diagnostic`.
/// Never an error: a stale default costs a trader one explicit
/// `@source`, and rejecting the whole config reload over it would be out
/// of all proportion.
pub fn default_source_diagnostic(config: &Config) -> Vec<Diagnostic> {
    let Some(v) = config.get("app", "timeseries.default_source") else {
        return vec![];
    };
    let warn = |message: String| Diagnostic {
        severity: Severity::Warning,
        layer: config.explain("app", "timeseries.default_source"),
        file: None,
        message,
        path: Some("app.timeseries.default_source".to_string()),
    };
    let Some(name) = v.as_str() else {
        return vec![warn(
            "[timeseries] default_source: expected a string naming a fetch source".to_string(),
        )];
    };
    let settings = SeriesSettings::from_config(config);
    if settings.dataset_of(name).is_some() {
        return vec![];
    }
    let have = settings.names().join(", ");
    vec![warn(format!(
        "[timeseries] default_source '{name}' names no fetch source (have: {have}); \
         `:add` needs an explicit @source"
    ))]
}

/// Write or remove `[timeseries] default_source` in `<user_dir>/app.toml`
/// through `config_write::edit`, the one door — preserving every other
/// table, key and comment, exactly as `linenumbers::persist_to_user_config`
/// does. `None` REMOVES the key rather than writing an empty string: the
/// settings row's `(none)` means "no default", which is the absent key,
/// so a trader who steps back to it leaves no drift behind.
pub fn persist_to_user_config(user_dir: &Path, source: Option<&str>) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("timeseries").is_some_and(Item::is_table_like) {
            // Nothing to remove from, and `(none)` must not create an
            // empty `[timeseries]` table in a trader's file.
            if source.is_none() {
                return;
            }
            doc["timeseries"] = Item::Table(Table::new());
        }
        let table = doc["timeseries"]
            .as_table_mut()
            .expect("just ensured [timeseries] is a table");
        match source {
            Some(s) => {
                table["default_source"] = value(s);
            }
            None => {
                table.remove("default_source");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, LayerDoc, Severity};

    const DATASETS: &str = "config_version = 1\n[series]\nfamily = \"series\"\n[risk]\n[risk.columns]\nbook = { type = \"utf8\", role = \"key\" }\npv = { type = \"f64\", role = \"value\" }\n";
    const SOURCES: &str = "config_version = 1\n[demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n[demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n[files]\ndataset = \"risk\"\npaths = [\"/tmp/*.csv\"]\n";

    fn config(app: &str) -> Config {
        Config::from_docs(vec![
            LayerDoc::builtin("datasets", DATASETS).unwrap(),
            LayerDoc::builtin("sources", SOURCES).unwrap(),
            LayerDoc::builtin("app", app).unwrap(),
        ])
    }

    #[test]
    fn the_fetch_sources_are_the_non_directory_sources_over_a_series_dataset() {
        let s = SeriesSettings::from_config(&config("config_version = 1\n"));
        assert_eq!(
            s.sources
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["demo_kdb", "demo_rest"]
        );
        assert_eq!(s.dataset_of("demo_rest"), Some("series"));
        assert_eq!(s.dataset_of("files"), None);
        assert_eq!(s.default_source, None);
    }

    #[test]
    fn the_default_source_is_read_and_diagnosed() {
        let s = SeriesSettings::from_config(&config(
            "config_version = 1\n[timeseries]\ndefault_source = \"demo_kdb\"\n",
        ));
        assert_eq!(s.default_source.as_deref(), Some("demo_kdb"));
        assert!(
            default_source_diagnostic(&config(
                "config_version = 1\n[timeseries]\ndefault_source = \"demo_kdb\"\n"
            ))
            .is_empty()
        );
        let d = default_source_diagnostic(&config(
            "config_version = 1\n[timeseries]\ndefault_source = \"nope\"\n",
        ));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].path.as_deref(), Some("app.timeseries.default_source"));
        assert!(
            d[0].message.contains("'nope'") && d[0].message.contains("demo_kdb, demo_rest"),
            "{}",
            d[0].message
        );
        let d = default_source_diagnostic(&config(
            "config_version = 1\n[timeseries]\ndefault_source = 3\n",
        ));
        assert!(d[0].message.contains("string"));
        assert!(
            default_source_diagnostic(&config("config_version = 1\n")).is_empty(),
            "absent is fine"
        );
    }

    #[test]
    fn persist_writes_and_removes_the_key_preserving_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            "# keep me\n[blotter]\nstale_after = \"15m\"\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), Some("demo_kdb")).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(
            text.contains("# keep me")
                && text.contains("[timeseries]")
                && text.contains("default_source = \"demo_kdb\""),
            "{text}"
        );
        persist_to_user_config(dir.path(), None).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(!text.contains("default_source"), "{text}");
        assert!(text.contains("stale_after"));
    }

    #[test]
    fn a_none_persist_creates_no_timeseries_table() {
        // Stepping back to `(none)` on a file that never had the key
        // must leave the file as it was, not add an empty table.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.toml"), "config_version = 1\n").unwrap();
        persist_to_user_config(dir.path(), None).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(!text.contains("[timeseries]"), "{text}");
    }
}
