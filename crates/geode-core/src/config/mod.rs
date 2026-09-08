//! Layered configuration (spec §8): Builtin → Desk → User TOML documents,
//! deep-merged with per-path provenance. Invalid config never panics —
//! failures surface as [`Diagnostic`] values and the bad input is skipped.

mod load;
mod merge;

pub use load::{load_layer, load_views};
pub use merge::{MergedDoc, merge_docs};

use std::path::PathBuf;

/// Config schema version accepted by this build.
pub const CONFIG_VERSION: i64 = 1;

/// Precedence order: later layers override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Builtin,
    Desk,
    User,
}

impl Layer {
    pub fn name(self) -> &'static str {
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

/// A problem found while loading or interpreting config. Never fatal.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub layer: Option<Layer>,
    pub file: Option<PathBuf>,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    /// `[layer] file: message`, degrading cleanly when either is absent:
    /// `[user] /path/keymap.toml: parse error` (both present), `[builtin]
    /// <no file>: message` (file absent), `<no file>: message` (both
    /// absent) — the leading `[layer] ` is simply omitted rather than
    /// printing an empty bracket pair.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(layer) = self.layer {
            write!(f, "[{}] ", layer.name())?;
        }
        match &self.file {
            Some(file) => write!(f, "{}: ", file.display()),
            None => write!(f, "<no file>: "),
        }?;
        write!(f, "{}", self.message)
    }
}

impl Diagnostic {
    pub fn error(layer: Layer, file: PathBuf, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            layer: Some(layer),
            file: Some(file),
            message: message.into(),
        }
    }

    pub fn warning(layer: Layer, file: PathBuf, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            layer: Some(layer),
            file: Some(file),
            message: message.into(),
        }
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

/// The loaded, merged configuration plus everything needed to explain it.
#[derive(Debug, Default)]
pub struct Config {
    docs: BTreeMap<String, MergedDoc>,
    layered: BTreeMap<String, Vec<LayerDoc>>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Config {
    pub fn load(sources: &ConfigSources) -> Config {
        let mut diagnostics = Vec::new();
        let mut all: Vec<LayerDoc> = sources.builtin.clone();
        for (layer, dir) in [(Layer::Desk, &sources.desk), (Layer::User, &sources.user)] {
            if let Some(dir) = dir {
                let (docs, diags) = load_layer(layer, dir);
                all.extend(docs);
                diagnostics.extend(diags);
            }
        }
        let mut layered: BTreeMap<String, Vec<LayerDoc>> = BTreeMap::new();
        for doc in all {
            layered.entry(doc.name.clone()).or_default().push(doc);
        }
        let docs = layered
            .iter()
            .map(|(name, docs)| (name.clone(), merge_docs(name, docs)))
            .collect();
        Config {
            docs,
            layered,
            diagnostics,
        }
    }

    pub fn doc(&self, name: &str) -> Option<&MergedDoc> {
        self.docs.get(name)
    }

    /// The unmerged per-layer docs for `name`, in Builtin → Desk → User order.
    /// Consumers that layer at interpretation time (the keymap engine) use
    /// this instead of the merged doc.
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

    // --- Diagnostic Display -------------------------------------------

    #[test]
    fn display_with_layer_and_file() {
        let diag = Diagnostic::error(Layer::User, PathBuf::from("/path/keymap.toml"), "bad toml");
        assert_eq!(diag.to_string(), "[user] /path/keymap.toml: bad toml");
    }

    #[test]
    fn display_with_layer_and_no_file() {
        let diag = Diagnostic {
            severity: Severity::Error,
            layer: Some(Layer::Builtin),
            file: None,
            message: "builtin doc invalid".to_string(),
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
}
