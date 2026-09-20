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
///
/// `PartialEq` (Phase 4b Task 4 fix round 1, MAJ-5): `Diagnostics::
/// note_config` compares a freshly loaded batch against the one already
/// held to decide whether a reload actually changed anything — `Severity`,
/// `Layer`, `PathBuf`, `String` and `Option<String>` (`path`) all already
/// support it, so this is a plain derive, not a new comparison to design.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub layer: Option<Layer>,
    pub file: Option<PathBuf>,
    pub message: String,
    /// The reader's own key path into the doc, e.g. `"app.theme.name"`
    /// (Phase 4b Task 4; grammar and every reader filled in Phase 4c
    /// §19.5: `<doc>.<object>[.<field>[.<index>[.<subkey>]]]`, e.g.
    /// `views.tree.columns.1.format.precision`). `None` by default —
    /// `Diagnostic::error`/`warning` and a handful of document-level
    /// literal build sites (a bad `default =` header, an unrecognised
    /// top-level key) leave it unset since there is no object to name —
    /// but every reader across `geode-core` that parses a *named* config
    /// object (views, view presentation, groupings, scopes, sources,
    /// datasets/schema, dimensions) attaches it via [`Self::with_path`].
    /// 4c's config dialogs use it to land a diagnostic on the field row
    /// it names (`Draft::row_for_path`, `flagged_rows`).
    pub path: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    /// `[layer] file: message`, degrading cleanly when either is absent:
    /// `[user] /path/keymap.toml: parse error` (both present), `[builtin]
    /// <no file>: message` (file absent), `<no file>: message` (both
    /// absent) — the leading `[layer] ` is simply omitted rather than
    /// printing an empty bracket pair. A `path`, when present, is
    /// appended as ` (at <path>)` after the message — it names the exact
    /// key the reader was looking at, which the layer/file pair alone
    /// cannot (two objects in the same file, one diagnostic each, read
    /// identically without it).
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

    /// Attach the reader's own key path into the doc (Phase 4b Task 4;
    /// consumed by 4c's config dialogs — see the `path` field's own doc
    /// comment). A builder, not a constructor parameter: every existing
    /// call site of `error`/`warning` stays unchanged.
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

/// The loaded, merged configuration plus everything needed to explain it.
///
/// `Clone` (Phase 4b Task 5): `geode_diagnostics::DiagnosticsFactory`
/// holds its own `Rc<RefCell<Config>>` for the config section's
/// effective-config explainer, refreshed on every `ShellEvent::
/// ConfigReloaded` from `ShellView::config()`'s `&Config` — the same
/// clone-on-reload shape `BlotterFactory::set_views`/`set_schema` already
/// use for their own `Vec`/`SchemaSpec` copies. Every field here is
/// already `Clone` (`MergedDoc`, `LayerDoc`, `Diagnostic`), so this is a
/// plain derive, not a new copy to design.
#[derive(Debug, Clone, Default)]
pub struct Config {
    docs: BTreeMap<String, MergedDoc>,
    layered: BTreeMap<String, Vec<LayerDoc>>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Config {
    /// Read every layer off disk, then merge — the two halves, in that
    /// order, and nothing else.
    ///
    /// The signature is unchanged and so is the behaviour; what changed
    /// (Phase 4c, spec §7.1) is that the halves are separable.
    /// [`read_docs`](Self::read_docs) is the disk half and
    /// [`from_docs`](Self::from_docs) the merge half, so a caller that
    /// already holds the documents — a config dialog that has just
    /// changed one object in memory — re-merges without a file read,
    /// through **the same merge** this function uses.
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
                // `load_layer` IS `read_layer_docs`: the disk half was
                // already a function of its own, so the split only had to
                // lift the merge out beside it rather than rename it and
                // churn every caller and test that names it.
                let (docs, diags) = load_layer(layer, dir);
                all.extend(docs);
                diagnostics.extend(diags);
            }
        }
        (all, diagnostics)
    }

    /// The merge half of [`load`](Self::load), and **the only place
    /// merging happens anywhere**.
    ///
    /// `docs` arrive in merge order (Builtin → Desk → User); they are
    /// grouped by name preserving that order, and each group is handed to
    /// [`merge_docs`]. `diagnostics` is empty — they belong to whatever
    /// produced the documents, and a caller that carries some forward
    /// (the dialogs' in-memory edit path, which re-read nothing and so
    /// has learned nothing new about the files) assigns them afterwards.
    ///
    /// Every source of a `Config` funnels through here — startup, the
    /// watcher's reload, and a dialog edit — so a dialog cannot disagree
    /// with the file it wrote about what its own change means. Adding a
    /// second merger to "optimise" an in-memory edit would reintroduce
    /// exactly that disagreement, silently.
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

    /// Every layered document this config was merged from, in merge
    /// order — what [`from_docs`](Self::from_docs) takes back.
    ///
    /// Grouped-by-name order rather than the flat order `read_docs`
    /// returned, which is the same thing as far as the merge is
    /// concerned: `from_docs` regroups by name, and only the order
    /// *within* a name's group decides anything.
    pub fn all_docs(&self) -> Vec<LayerDoc> {
        self.layered.values().flatten().cloned().collect()
    }

    pub fn doc(&self, name: &str) -> Option<&MergedDoc> {
        self.docs.get(name)
    }

    /// Every doc name this `Config` holds a merged doc for, in
    /// alphabetical order (`docs` is a `BTreeMap`) — for a reader that
    /// wants to enumerate "everything", not name one doc in particular
    /// (Phase 4b Task 5 fix round 1, MIN-3: the diagnostics module's
    /// effective-config explainer used to walk a hand-maintained constant
    /// list of doc names instead, which could silently fall behind a doc
    /// this or a future reader added). This is about *display*, not the
    /// "a reader names what it wants" rationale `doc`/`get` follow for
    /// *typed* access — an enumerator doesn't weaken that.
    pub fn doc_names(&self) -> impl Iterator<Item = &str> {
        self.docs.keys().map(String::as_str)
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

/// Fixture builders for downstream crates' tests that need a real
/// `Config` — not just a `MergedDoc` (`merge_docs`/`LayerDoc::builtin`,
/// the pattern `geode-data`'s benches use) — but with no desk/user
/// directory on disk.
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

/// Whether `name` can be the top-level key of an object in a layered
/// config doc (`[name]` in `views.toml`, `name = [...]` in
/// `groupings.toml`): trimmed, non-empty, not the `config_version`
/// stamp every doc carries, and free of the three characters that would
/// make it a quoted or dotted TOML key (whitespace, `.`, `"`).
///
/// One rule, two callers — `Frame::save_scope` (historically `:scope
/// save <name>` on a tile's command line, test-only since the
/// 2026-09-20 command-line locality ruling retired that route) and the
/// object dialog's `n` — so a name the dialog accepts is a name
/// `Frame::save_scope` still accepts too, and vice versa. Returns the
/// trimmed name so a caller cannot check one spelling and write
/// another.
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

    /// The merge half alone, on the docs the disk half produced, is the
    /// same `Config` `load` builds — the property that lets an in-memory
    /// edit (the config dialogs, spec §7.1) re-merge without going near a
    /// file. A second merger would be free to disagree; this asserts the
    /// one that exists is the one both paths use.
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

    /// Phase 4b Task 5 fix round 1, MIN-3: `doc_names` lists every doc
    /// `Config` actually holds, alphabetically — so an effective-config
    /// explainer walking it needs no hand-maintained list to keep in
    /// sync with what's actually loaded.
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

    /// Phase 4b Task 4: `path` defaults to `None` on every constructor
    /// (`error`/`warning`) and `with_path` is the one door that sets it —
    /// 4c's config dialogs attach a reader's key path to a field row
    /// through this builder.
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
