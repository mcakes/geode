//! Layered TOML configuration with recursive table merging, whole-object
//! replacement for named definitions, and dotted-path provenance.
//!
//! Disk loading collects read, parse, and version diagnostics. Typed readers
//! validate the merged documents separately.

mod load;
mod merge;

pub use load::{load_layer, load_views};
pub use merge::{MergedDoc, merge_docs};

use std::path::PathBuf;

/// Config schema version accepted by this build.
pub const CONFIG_VERSION: i64 = 1;

/// The named-color document, `colors.toml`.
pub const COLORS_DOC: &str = "colors";

/// The named scope expressions document, `expressions.toml`.
pub const EXPRESSIONS_DOC: &str = "expressions";

/// Documents whose file was renamed: `(old stem, current doc name)`. A layer
/// directory still holding only the old file loads it under the current name
/// with a warning; when a layer holds both, the current file wins and the old
/// one is ignored with a warning. Writes always target the current name
/// (`geode_shell::config_write` carries an old user file across on its first
/// edit), so an existing hand-written file is never silently discarded.
pub const RENAMED_DOCS: &[(&str, &str)] = &[("colours", COLORS_DOC)];

/// The current document name for a renamed file stem, if `stem` is an old one.
pub fn renamed_doc(stem: &str) -> Option<&'static str> {
    RENAMED_DOCS
        .iter()
        .find(|(old, _)| *old == stem)
        .map(|(_, new)| *new)
}

/// The old file stem a current document was renamed from, if any.
pub fn legacy_doc_name(doc: &str) -> Option<&'static str> {
    RENAMED_DOCS
        .iter()
        .find(|(_, new)| *new == doc)
        .map(|(old, _)| *old)
}

/// Precedence order: later layers override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Builtin,
    Desk,
    User,
}

impl Layer {
    pub const fn name(self) -> &'static str {
        match self {
            Layer::Builtin => "builtin",
            Layer::Desk => "desk",
            Layer::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A problem found while loading or interpreting configuration. Consumers
/// choose whether its severity rejects an operation. Equality includes location
/// and message, allowing unchanged diagnostic batches to be deduplicated.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub layer: Option<Layer>,
    pub file: Option<PathBuf>,
    pub message: String,
    /// Optional reader-assigned key path, such as
    /// `views.tree.columns.1.format.precision`. Dialogs use it to associate a
    /// diagnostic with a field row; document-level errors may omit it.
    pub path: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    /// Display `[layer] file: message`, omitting an absent layer and using
    /// `<no file>` for an absent file. Append ` (at <path>)` when supplied.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(layer) = self.layer {
            write!(f, "[{}] ", layer.name())?;
        }
        match &self.file {
            Some(file) => write!(f, "{}: ", file.display()),
            None => write!(f, "<no file>: "),
        }?;
        write!(f, "{}", self.message)?;
        if let Some(path) = &self.path {
            write!(f, " (at {path})")?;
        }
        Ok(())
    }
}

impl Diagnostic {
    pub fn error(layer: Layer, file: PathBuf, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            layer: Some(layer),
            file: Some(file),
            message: message.into(),
            path: None,
        }
    }

    pub fn warning(layer: Layer, file: PathBuf, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            layer: Some(layer),
            file: Some(file),
            message: message.into(),
            path: None,
        }
    }

    /// Attach the reader's key path for field-level diagnostic display.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// One parsed TOML document from one layer. The file stem is the doc name:
/// `keymap.toml` → doc "keymap".
#[derive(Debug, Clone)]
pub struct LayerDoc {
    pub layer: Layer,
    pub name: String,
    pub file: PathBuf,
    pub table: toml::Table,
}

impl LayerDoc {
    /// A compiled-in builtin default document. Builtin docs skip the
    /// `config_version` check — they are authored with the binary.
    pub fn builtin(name: &str, text: &str) -> Result<LayerDoc, Diagnostic> {
        let table = text.parse::<toml::Table>().map_err(|e| Diagnostic {
            severity: Severity::Error,
            layer: Some(Layer::Builtin),
            file: None,
            message: format!("builtin doc '{name}': {e}"),
            path: None,
        })?;
        Ok(LayerDoc {
            layer: Layer::Builtin,
            name: name.to_string(),
            file: PathBuf::from(format!("<builtin:{name}>")),
            table,
        })
    }
}

use std::collections::BTreeMap;

/// Where config comes from. Builtin docs are compiled in; desk and user are
/// directories of `*.toml` files (either may be absent).
#[derive(Debug, Default)]
pub struct ConfigSources {
    pub builtin: Vec<LayerDoc>,
    pub desk: Option<PathBuf>,
    pub user: Option<PathBuf>,
}

/// Merged documents, their original layer documents, provenance, and diagnostics.
#[derive(Debug, Clone, Default)]
pub struct Config {
    docs: BTreeMap<String, MergedDoc>,
    layered: BTreeMap<String, Vec<LayerDoc>>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Config {
    /// Read builtin, desk, and user documents in precedence order, then merge
    /// and attach read diagnostics. Typed document validation is separate.
    /// Use [`Self::from_docs`] to merge documents already held in memory.
    pub fn load(sources: &ConfigSources) -> Config {
        let (docs, diagnostics) = Self::read_docs(sources);
        let mut config = Self::from_docs(docs);
        config.diagnostics = diagnostics;
        config
    }

    /// The disk half of [`load`](Self::load): the builtin docs the binary
    /// carries, then every `*.toml` the desk and user directories hold,
    /// in merge order. No merging happens here.
    pub fn read_docs(sources: &ConfigSources) -> (Vec<LayerDoc>, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        let mut all: Vec<LayerDoc> = sources.builtin.clone();
        for (layer, dir) in [(Layer::Desk, &sources.desk), (Layer::User, &sources.user)] {
            if let Some(dir) = dir {
                let (docs, diags) = load_layer(layer, dir);
                all.extend(docs);
                diagnostics.extend(diags);
            }
        }
        (all, diagnostics)
    }

    /// Merge documents grouped by name, preserving each group's input order.
    /// The caller supplies precedence order; this method does not sort by layer.
    /// Diagnostics start empty, so callers must attach any read diagnostics they
    /// need to retain. Typed readers validate the resulting documents separately.
    pub fn from_docs(docs: Vec<LayerDoc>) -> Config {
        let mut layered: BTreeMap<String, Vec<LayerDoc>> = BTreeMap::new();
        for doc in docs {
            layered.entry(doc.name.clone()).or_default().push(doc);
        }
        let docs = layered
            .iter()
            .map(|(name, docs)| (name.clone(), merge_docs(name, docs)))
            .collect();
        Config {
            docs,
            layered,
            diagnostics: Vec::new(),
        }
    }

    /// Return original documents grouped alphabetically by name, preserving
    /// merge order within each group. Passing these to [`Self::from_docs`]
    /// reproduces the merged documents, without carrying over diagnostics.
    pub fn all_docs(&self) -> Vec<LayerDoc> {
        self.layered.values().flatten().cloned().collect()
    }

    pub fn doc(&self, name: &str) -> Option<&MergedDoc> {
        self.docs.get(name)
    }

    /// Iterate the names of all merged documents in alphabetical order.
    pub fn doc_names(&self) -> impl Iterator<Item = &str> {
        self.docs.keys().map(String::as_str)
    }

    /// The unmerged documents for `name`, in supplied merge order (normally
    /// Builtin → Desk → User). Readers such as the keymap engine interpret
    /// these layers directly.
    pub fn layered_docs(&self, name: &str) -> &[LayerDoc] {
        self.layered.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Dotted-path lookup into a merged doc: `get("app", "keymap.mod")`.
    pub fn get(&self, doc: &str, path: &str) -> Option<&toml::Value> {
        let merged = self.docs.get(doc)?;
        let mut parts = path.split('.');
        let mut current = merged.value.get(parts.next()?)?;
        for part in parts {
            current = current.as_table()?.get(part)?;
        }
        Some(current)
    }

    /// Which layer supplied the value at `path` (exact entry or nearest
    /// recorded ancestor).
    pub fn explain(&self, doc: &str, path: &str) -> Option<Layer> {
        let merged = self.docs.get(doc)?;
        let mut probe = path.to_string();
        loop {
            if let Some(layer) = merged.provenance.get(&probe) {
                return Some(*layer);
            }
            match probe.rfind('.') {
                Some(i) => probe.truncate(i),
                None => return None,
            }
        }
    }
}

/// In-memory configuration fixtures for downstream tests.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::{Config, ConfigSources, LayerDoc};

    /// A `Config` built from one builtin doc named `name`, parsed from
    /// `text`. Builtin docs skip the `config_version` check (see
    /// [`LayerDoc::builtin`]), so `text` need not carry one.
    pub fn config_from(name: &str, text: &str) -> Config {
        let doc = LayerDoc::builtin(name, text).expect("well-formed test TOML");
        Config::load(&ConfigSources {
            builtin: vec![doc],
            desk: None,
            user: None,
        })
    }
}

/// Trim and validate a layered-config object name. Reject empty names,
/// `config_version`, whitespace, dots, and double quotes. Return the trimmed
/// spelling so callers validate and persist the same name.
pub fn check_object_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name == "config_version"
        || name.contains(|c: char| c.is_whitespace() || c == '.' || c == '"')
    {
        return Err(format!("'{name}' is not a usable name"));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn load_merges_three_layers_in_order() {
        let desk = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write(
            desk.path(),
            "app.toml",
            "config_version = 1\n[keymap]\nmod = \"ctrl\"\n",
        );
        write(
            user.path(),
            "app.toml",
            "config_version = 1\n[keymap]\nmod = \"cmd\"\n",
        );
        let sources = ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n")
                    .unwrap(),
            ],
            desk: Some(desk.path().to_path_buf()),
            user: Some(user.path().to_path_buf()),
        };
        let config = Config::load(&sources);
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        assert_eq!(
            config.get("app", "keymap.mod").unwrap().as_str(),
            Some("cmd")
        );
        assert_eq!(
            config.get("app", "theme.name").unwrap().as_str(),
            Some("dark")
        );
        assert_eq!(config.explain("app", "keymap.mod"), Some(Layer::User));
        assert_eq!(config.explain("app", "theme.name"), Some(Layer::Builtin));
        let layers: Vec<_> = config.layered_docs("app").iter().map(|d| d.layer).collect();
        assert_eq!(layers, vec![Layer::Builtin, Layer::Desk, Layer::User]);
    }

    /// Merging the disk reader's documents must produce the same values and
    /// provenance as `Config::load`.
    #[test]
    fn from_docs_merges_exactly_as_load_does() {
        let desk = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write(
            desk.path(),
            "app.toml",
            "config_version = 1\n[keymap]\nmod = \"ctrl\"\n",
        );
        write(
            user.path(),
            "app.toml",
            "config_version = 1\n[keymap]\nmod = \"cmd\"\n",
        );
        let sources = ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n")
                    .unwrap(),
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
            ],
            desk: Some(desk.path().to_path_buf()),
            user: Some(user.path().to_path_buf()),
        };
        let loaded = Config::load(&sources);
        let remerged = Config::from_docs(loaded.all_docs());
        for name in ["app", "views"] {
            assert_eq!(
                remerged.doc(name).map(|d| d.value.clone()),
                loaded.doc(name).map(|d| d.value.clone()),
                "{name} merged differently"
            );
            assert_eq!(
                remerged.doc(name).map(|d| d.provenance.clone()),
                loaded.doc(name).map(|d| d.provenance.clone()),
                "{name}'s provenance differs"
            );
            let layers: Vec<_> = remerged
                .layered_docs(name)
                .iter()
                .map(|d| (d.layer, d.name.clone(), d.file.clone(), d.table.clone()))
                .collect();
            let expected: Vec<_> = loaded
                .layered_docs(name)
                .iter()
                .map(|d| (d.layer, d.name.clone(), d.file.clone(), d.table.clone()))
                .collect();
            assert_eq!(layers, expected, "{name}'s layered docs differ");
        }
    }

    #[test]
    fn explain_falls_back_to_nearest_ancestor() {
        let sources = ConfigSources {
            builtin: vec![LayerDoc::builtin("views", "[risk]\ndataset = \"risk\"\n").unwrap()],
            desk: None,
            user: None,
        };
        let config = Config::load(&sources);
        // "views" is atomic at depth 1, so provenance is recorded on "risk";
        // asking about a leaf inside it resolves via the ancestor.
        assert_eq!(
            config.explain("views", "risk.dataset"),
            Some(Layer::Builtin)
        );
    }

    /// Document enumeration includes every loaded name in alphabetical order.
    #[test]
    fn doc_names_lists_every_loaded_doc_alphabetically() {
        let sources = ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[risk]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("app", "config_version = 1\n").unwrap(),
                LayerDoc::builtin("keymap", "").unwrap(),
            ],
            desk: None,
            user: None,
        };
        let config = Config::load(&sources);
        let names: Vec<&str> = config.doc_names().collect();
        assert_eq!(names, vec!["app", "keymap", "views"]);
    }

    // --- Diagnostic Display -------------------------------------------

    #[test]
    fn display_with_layer_and_file() {
        let diag = Diagnostic::error(Layer::User, PathBuf::from("/path/keymap.toml"), "bad toml");
        assert_eq!(diag.to_string(), "[user] /path/keymap.toml: bad toml");
    }

    /// Constructors omit the key path; `with_path` attaches it.
    #[test]
    fn with_path_sets_the_field_and_defaults_to_none() {
        let diag = Diagnostic::error(Layer::User, PathBuf::from("app.toml"), "bad");
        assert_eq!(diag.path, None);
        let diag = diag.with_path("app.theme.name");
        assert_eq!(diag.path.as_deref(), Some("app.theme.name"));
    }

    #[test]
    fn display_with_layer_and_no_file() {
        let diag = Diagnostic {
            severity: Severity::Error,
            layer: Some(Layer::Builtin),
            file: None,
            message: "builtin doc invalid".to_string(),
            path: None,
        };
        assert_eq!(diag.to_string(), "[builtin] <no file>: builtin doc invalid");
    }

    #[test]
    fn display_with_file_and_no_layer() {
        let diag = Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: Some(PathBuf::from("app.toml")),
            message: "unrecognized key".to_string(),
            path: None,
        };
        assert_eq!(diag.to_string(), "app.toml: unrecognized key");
    }

    #[test]
    fn display_with_neither_layer_nor_file() {
        let diag = Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            message: "generic warning".to_string(),
            path: None,
        };
        assert_eq!(diag.to_string(), "<no file>: generic warning");
    }

    #[test]
    fn absent_layers_and_docs_are_fine() {
        let config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: None,
            user: None,
        });
        assert!(config.doc("nope").is_none());
        assert!(config.layered_docs("nope").is_empty());
        assert!(config.get("nope", "a.b").is_none());
    }

    // --- Object names -----------------------------------------------

    #[test]
    fn object_names_follow_the_frames_rule() {
        assert_eq!(check_object_name("  my_books "), Ok("my_books"));
        assert_eq!(check_object_name("tree-2"), Ok("tree-2"));
        for bad in ["", "   ", "config_version", "a b", "a.b", "a\"b"] {
            assert!(check_object_name(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
