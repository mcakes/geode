# Geode Phase 1a (Shell Logic) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The shell's pure-logic core: layered TOML configuration with provenance, the action registry, and the full keymap engine (keystroke parsing, context predicates, layered binding resolution, sequence-aware matching) — all unit-tested without a window.

**Architecture:** Config machinery lives in `geode-core` (spec §2: config model is shared vocabulary). Actions and keymap live in `geode-shell`. Nothing in this plan touches gpui — Phase 1b wires this logic into the UI. Config layers merge Builtin → Desk → User with per-path provenance (spec §8); the keymap engine consumes *unmerged* per-layer docs so later layers override or unbind earlier bindings at match time (spec §3.4).

**Tech Stack:** Rust, `toml` crate (parsing), `tempfile` (dev-only, filesystem tests). Predicate and keystroke parsers are hand-rolled — no parser dependencies.

**Spec:** `docs/superpowers/specs/2026-08-28-geode-foundation-design.md` — §3.4 (keymap engine), §8 (configuration model), §10.1 (user errors are inline diagnostics, never crashes), §10.3 (shell logic is pure and window-free).

## Global Constraints

- No gpui: neither crate touched here may gain a gpui dependency in this plan.
- Crate placement per spec §2: config → `geode-core`; actions/keymap → `geode-shell`. `geode-shell` may depend on `geode-core`, never on `geode-data` or modules.
- New dependencies allowed: `toml` (geode-core, regular), `tempfile` (both crates, dev-only). Nothing else.
- Invalid config never panics: every failure path produces a `Diagnostic` value and degrades (bad file skipped, bad binding skipped), per spec §8/§10.1.
- Both platforms must keep working (all code here is platform-neutral std Rust; CI enforces).
- Workspace invariant: any new lib/bin target needs `bench = false` — this plan adds no new crates or targets, so no action needed.
- All commits end with the project's standard co-author trailer (as in the existing git history).
- TDD per house rules: tests written first in every task, watched failing, then implemented.

## File Structure

```
crates/geode-core/src/config/mod.rs        types (Layer, Diagnostic, LayerDoc), Config API, ConfigSources
crates/geode-core/src/config/load.rs       per-layer *.toml directory loading + version check
crates/geode-core/src/config/merge.rs      deep merge, atomic named objects, provenance
crates/geode-shell/src/actions.rs          ActionId, ActionDef, ActionRegistry
crates/geode-shell/src/keymap/mod.rs       module wiring + re-exports
crates/geode-shell/src/keymap/keystroke.rs Modifiers, Keystroke, parse_keystroke, parse_binding
crates/geode-shell/src/keymap/context.rs   KeyContext, Predicate, parse_predicate, eval
crates/geode-shell/src/keymap/build.rs     Binding, Keymap, build_keymap (from layered config docs)
crates/geode-shell/src/keymap/matcher.rs   Matcher, MatchResult (sequences, precedence, unbind)
crates/geode-shell/src/defaults.rs         builtin actions + builtin keymap doc + mod-alias resolution
crates/geode-shell/tests/keymap_integration.rs   full stack: config dirs → keymap → matcher
```

Conventions fixed by this plan (used verbatim throughout):

- Action id strings are `module::action`, e.g. `workspace::focus_left`.
- A binding string is whitespace-separated keystrokes; a keystroke is `+`-separated modifiers then one key: `mod+shift+h`, `g g`, `ctrl+1`. Keys are stored lowercase; shift is always explicit (`shift+g`, never `G`).
- `mod` is an alias resolved at parse time (default Alt; user-configurable via the `app` doc, key `keymap.mod`, values `"alt" | "ctrl" | "cmd"`).
- The action string `"none"` unbinds: a higher layer binding a key to `none` makes it match nothing.
- Config docs are `*.toml` files (non-recursive) in a layer directory; the file stem is the doc name. Each hand-written file carries `config_version = 1`.

---

### Task 1: Config types and layer loading

**Files:**
- Modify: `crates/geode-core/Cargo.toml` (add deps)
- Modify: `crates/geode-core/src/lib.rs` (declare module)
- Create: `crates/geode-core/src/config/mod.rs`
- Create: `crates/geode-core/src/config/load.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 2, 6, 8): `Layer` (Builtin|Desk|User, Ord), `Severity` (Warning|Error), `Diagnostic { severity, layer: Option<Layer>, file: Option<PathBuf>, message: String }` with constructors `Diagnostic::error(layer, file, msg)` / `Diagnostic::warning(layer, file, msg)`, `LayerDoc { layer, name, file, table: toml::Table }`, `LayerDoc::builtin(name, text) -> Result<LayerDoc, Diagnostic>`, `load_layer(layer, root: &Path) -> (Vec<LayerDoc>, Vec<Diagnostic>)`, `CONFIG_VERSION: i64 = 1`.

- [ ] **Step 1: Add dependencies**

Run: `cargo add toml -p geode-core && cargo add --dev tempfile -p geode-core`

- [ ] **Step 2: Write module skeleton and types**

In `crates/geode-core/src/lib.rs`, replace the line `//! Intentionally near-empty in phase 0.` with:
```rust
pub mod config;
```

`crates/geode-core/src/config/mod.rs`:
```rust
//! Layered configuration (spec §8): Builtin → Desk → User TOML documents,
//! deep-merged with per-path provenance. Invalid config never panics —
//! failures surface as [`Diagnostic`] values and the bad input is skipped.

mod load;

pub use load::load_layer;

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
```

- [ ] **Step 3: Write the failing tests for loading**

`crates/geode-core/src/config/load.rs`:
```rust
use super::{Diagnostic, Layer, LayerDoc, Severity, CONFIG_VERSION};
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn loads_docs_by_file_stem_sorted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "keymap.toml", "config_version = 1\n");
        write(dir.path(), "app.toml", "config_version = 1\n[keymap]\nmod = \"alt\"\n");
        write(dir.path(), "notes.txt", "ignored");
        let (docs, diags) = load_layer(Layer::User, dir.path());
        assert!(diags.is_empty(), "{diags:?}");
        let names: Vec<_> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["app", "keymap"]);
        assert!(docs.iter().all(|d| d.layer == Layer::User));
    }

    #[test]
    fn missing_directory_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let (docs, diags) = load_layer(Layer::Desk, &missing);
        assert!(docs.is_empty());
        assert!(diags.is_empty());
    }

    #[test]
    fn invalid_toml_is_error_diagnostic_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bad.toml", "this is [not toml");
        write(dir.path(), "good.toml", "config_version = 1\n");
        let (docs, diags) = load_layer(Layer::User, dir.path());
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].name, "good");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].file.as_ref().unwrap().ends_with("bad.toml"));
    }

    #[test]
    fn wrong_config_version_is_error_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "future.toml", "config_version = 99\n");
        let (docs, diags) = load_layer(Layer::User, dir.path());
        assert!(docs.is_empty());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("config_version"));
    }

    #[test]
    fn missing_config_version_is_warning_but_loaded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "loose.toml", "[keymap]\nmod = \"ctrl\"\n");
        let (docs, diags) = load_layer(Layer::User, dir.path());
        assert_eq!(docs.len(), 1);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p geode-core`
Expected: compile error — `load_layer` not defined.

- [ ] **Step 5: Implement loading**

Insert above the tests in `crates/geode-core/src/config/load.rs`:
```rust
/// Read every `*.toml` file in `root` (non-recursive, sorted by path).
/// A missing directory is not an error — a layer may simply be absent.
pub fn load_layer(layer: Layer, root: &Path) -> (Vec<LayerDoc>, Vec<Diagnostic>) {
    let mut docs = Vec::new();
    let mut diags = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (docs, diags);
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                diags.push(Diagnostic::error(layer, path, format!("unreadable: {e}")));
                continue;
            }
        };
        let table = match text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                diags.push(Diagnostic::error(layer, path, format!("parse error: {e}")));
                continue;
            }
        };
        match table.get("config_version") {
            Some(toml::Value::Integer(v)) if *v == CONFIG_VERSION => {}
            Some(other) => {
                diags.push(Diagnostic::error(
                    layer,
                    path,
                    format!("unsupported config_version {other} (this build supports {CONFIG_VERSION})"),
                ));
                continue;
            }
            None => {
                diags.push(Diagnostic::warning(
                    layer,
                    path.clone(),
                    format!("missing config_version (assuming {CONFIG_VERSION})"),
                ));
            }
        }
        let name = path
            .file_stem()
            .expect("filtered to *.toml above")
            .to_string_lossy()
            .into_owned();
        docs.push(LayerDoc { layer, name, file: path, table });
    }
    (docs, diags)
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p geode-core`
Expected: 5 tests PASS.

- [ ] **Step 7: Lint, format, commit**

Run: `cargo clippy -p geode-core --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-core Cargo.lock
git commit -m "feat: config layer types and per-layer TOML document loading"
```

---

### Task 2: Deep merge, provenance, and the Config API

**Files:**
- Create: `crates/geode-core/src/config/merge.rs`
- Modify: `crates/geode-core/src/config/mod.rs`

**Interfaces:**
- Consumes: Task 1's types.
- Produces (used by Tasks 6, 8 and later phases):
  - `MergedDoc { value: toml::Table, provenance: BTreeMap<String, Layer> }`
  - `merge_docs(name: &str, layered: &[LayerDoc]) -> MergedDoc`
  - `ConfigSources { builtin: Vec<LayerDoc>, desk: Option<PathBuf>, user: Option<PathBuf> }`
  - `Config::load(&ConfigSources) -> Config` with `Config::doc(name) -> Option<&MergedDoc>`, `Config::layered_docs(name) -> &[LayerDoc]`, `Config::get(doc, dotted_path) -> Option<&toml::Value>`, `Config::explain(doc, dotted_path) -> Option<Layer>`, `Config::diagnostics: Vec<Diagnostic>`

- [ ] **Step 1: Write the failing merge tests**

`crates/geode-core/src/config/merge.rs`:
```rust
use super::{Layer, LayerDoc};
use std::collections::BTreeMap;
use toml::{Table, Value};

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(layer: Layer, name: &str, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: name.to_string(),
            file: format!("{}/{name}.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    #[test]
    fn later_layer_scalar_wins() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Desk, "app", "[keymap]\nmod = \"alt\"\n"),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(
            merged.value["keymap"]["mod"].as_str(),
            Some("ctrl")
        );
    }

    #[test]
    fn deep_merge_preserves_sibling_keys() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Desk, "app", "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n"),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.value["theme"]["name"].as_str(), Some("dark"));
        assert_eq!(merged.value["keymap"]["mod"].as_str(), Some("ctrl"));
    }

    #[test]
    fn atomic_doc_replaces_named_object_whole() {
        // "views" is an atomic doc: a later layer's view of the same name
        // replaces the earlier one entirely — no field-level merge (spec §8).
        let merged = merge_docs(
            "views",
            &[
                doc(Layer::Desk, "views", "[risk]\ndataset = \"risk\"\ncolumns = [\"npv\", \"delta\"]\n"),
                doc(Layer::User, "views", "[risk]\ndataset = \"risk\"\n"),
            ],
        );
        let risk = merged.value["risk"].as_table().unwrap();
        assert!(risk.get("columns").is_none(), "atomic override must drop desk-only fields");
    }

    #[test]
    fn non_atomic_arrays_are_replaced_not_appended() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Desk, "app", "recent = [1, 2]\n"),
                doc(Layer::User, "app", "recent = [3]\n"),
            ],
        );
        assert_eq!(merged.value["recent"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn provenance_tracks_winning_layer() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Builtin, "app", "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n"),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.provenance.get("keymap.mod"), Some(&Layer::User));
        assert_eq!(merged.provenance.get("theme.name"), Some(&Layer::Builtin));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p geode-core merge`
Expected: compile error — `merge_docs`, `MergedDoc` not defined. (Add `mod merge;` and `pub use merge::{merge_docs, MergedDoc};` to `config/mod.rs` first so the failure is the missing items, not the missing module.)

- [ ] **Step 3: Implement merge**

Insert above the tests in `merge.rs`:
```rust
/// A doc after layer merging, with per-dotted-path provenance for
/// `geode config explain`-style tooling (spec §8: "layered config without
/// provenance is a support nightmare").
#[derive(Debug, Clone, Default)]
pub struct MergedDoc {
    pub value: Table,
    pub provenance: BTreeMap<String, Layer>,
}

/// Docs whose top-level entries are named objects overridden whole-object
/// by name (spec §8): merging inside a view/layout is clever but undebuggable.
fn atomic_depth(doc_name: &str) -> Option<u32> {
    match doc_name {
        "views" | "layouts" | "groupings" | "scopes" => Some(1),
        _ => None,
    }
}

/// Merge layer docs in slice order (callers pass Builtin → Desk → User).
pub fn merge_docs(name: &str, layered: &[LayerDoc]) -> MergedDoc {
    let mut out = MergedDoc::default();
    let atomic = atomic_depth(name);
    for doc in layered {
        merge_table(
            &mut out.value,
            &doc.table,
            doc.layer,
            &mut out.provenance,
            "",
            0,
            atomic,
        );
    }
    out
}

fn merge_table(
    dst: &mut Table,
    src: &Table,
    layer: Layer,
    prov: &mut BTreeMap<String, Layer>,
    path: &str,
    depth: u32,
    atomic: Option<u32>,
) {
    for (key, value) in src {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        let entry_depth = depth + 1;
        let replace_whole = atomic == Some(entry_depth);
        match (dst.get_mut(key), value) {
            (Some(Value::Table(d)), Value::Table(s)) if !replace_whole => {
                merge_table(d, s, layer, prov, &child_path, entry_depth, atomic);
            }
            _ => {
                dst.insert(key.clone(), value.clone());
                record_provenance(prov, &child_path, layer);
            }
        }
    }
}

fn record_provenance(prov: &mut BTreeMap<String, Layer>, path: &str, layer: Layer) {
    let prefix = format!("{path}.");
    prov.retain(|p, _| p != path && !p.starts_with(&prefix));
    prov.insert(path.to_string(), layer);
}
```

- [ ] **Step 4: Run merge tests to verify they pass**

Run: `cargo test -p geode-core merge`
Expected: 5 tests PASS.

- [ ] **Step 5: Write the failing Config API tests**

Append to `crates/geode-core/src/config/mod.rs`:
```rust
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
        write(desk.path(), "app.toml", "config_version = 1\n[keymap]\nmod = \"ctrl\"\n");
        write(user.path(), "app.toml", "config_version = 1\n[keymap]\nmod = \"cmd\"\n");
        let sources = ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n").unwrap()],
            desk: Some(desk.path().to_path_buf()),
            user: Some(user.path().to_path_buf()),
        };
        let config = Config::load(&sources);
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        assert_eq!(config.get("app", "keymap.mod").unwrap().as_str(), Some("cmd"));
        assert_eq!(config.get("app", "theme.name").unwrap().as_str(), Some("dark"));
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
        assert_eq!(config.explain("views", "risk.dataset"), Some(Layer::Builtin));
    }

    #[test]
    fn absent_layers_and_docs_are_fine() {
        let config = Config::load(&ConfigSources { builtin: vec![], desk: None, user: None });
        assert!(config.doc("nope").is_none());
        assert!(config.layered_docs("nope").is_empty());
        assert!(config.get("nope", "a.b").is_none());
    }
}
```

- [ ] **Step 6: Run to verify failure, then implement the Config API**

Run: `cargo test -p geode-core config::tests`
Expected: compile error — `Config`, `ConfigSources` not defined.

Add to `crates/geode-core/src/config/mod.rs` (below the existing items, above the tests), plus `mod merge;` / `pub use merge::{merge_docs, MergedDoc};` if not already added in Step 2:
```rust
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
        Config { docs, layered, diagnostics }
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
```

- [ ] **Step 7: Run all geode-core tests to verify they pass**

Run: `cargo test -p geode-core`
Expected: 13 tests PASS (5 load + 5 merge + 3 config).

- [ ] **Step 8: Lint, format, commit**

Run: `cargo clippy -p geode-core --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-core
git commit -m "feat: layered config merge with provenance and the Config API"
```

---

### Task 3: Action registry

**Files:**
- Modify: `crates/geode-shell/src/lib.rs` (declare module)
- Create: `crates/geode-shell/src/actions.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 6, 8, and every future module): `ActionId(pub String)` (Ord, Hash, Display; convention `module::action`), `ActionDef { id: ActionId, title: String, category: String }`, `ActionRegistry` with `register(ActionDef) -> Result<(), String>` (Err on duplicate id), `get(&ActionId) -> Option<&ActionDef>`, `contains(&ActionId) -> bool`, `iter() -> impl Iterator<Item = &ActionDef>` (sorted by id).

- [ ] **Step 1: Declare the module**

In `crates/geode-shell/src/lib.rs`, replace the line `//! Intentionally near-empty in phase 0.` with:
```rust
pub mod actions;
```
(Keep the rest of the doc comment, including the dependency-rule line.)

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/actions.rs`:
```rust
use std::collections::BTreeMap;
use std::fmt;

#[cfg(test)]
mod tests {
    use super::*;

    fn def(id: &str, title: &str) -> ActionDef {
        ActionDef {
            id: ActionId(id.to_string()),
            title: title.to_string(),
            category: "Test".to_string(),
        }
    }

    #[test]
    fn register_and_get() {
        let mut reg = ActionRegistry::default();
        reg.register(def("workspace::focus_left", "Focus left")).unwrap();
        assert!(reg.contains(&ActionId("workspace::focus_left".into())));
        assert_eq!(
            reg.get(&ActionId("workspace::focus_left".into())).unwrap().title,
            "Focus left"
        );
        assert!(!reg.contains(&ActionId("workspace::nope".into())));
    }

    #[test]
    fn duplicate_registration_is_an_error() {
        let mut reg = ActionRegistry::default();
        reg.register(def("a::b", "First")).unwrap();
        let err = reg.register(def("a::b", "Second")).unwrap_err();
        assert!(err.contains("a::b"));
        assert_eq!(reg.get(&ActionId("a::b".into())).unwrap().title, "First");
    }

    #[test]
    fn iter_is_sorted_by_id() {
        let mut reg = ActionRegistry::default();
        reg.register(def("b::b", "B")).unwrap();
        reg.register(def("a::a", "A")).unwrap();
        let ids: Vec<_> = reg.iter().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, vec!["a::a", "b::b"]);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell`
Expected: compile error — types not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `actions.rs`:
```rust
//! The shared action registry (spec §3.2, §9.1): modules declare actions
//! here; the keymap maps keys to action ids; the palette lists them. Modules
//! never bind keys directly.

/// Stable action identifier, `module::action` by convention
/// (e.g. `workspace::focus_left`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionId(pub String);

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct ActionDef {
    pub id: ActionId,
    /// Human-readable, palette-facing: "Focus left".
    pub title: String,
    /// Palette grouping: "Workspace".
    pub category: String,
}

#[derive(Debug, Default)]
pub struct ActionRegistry {
    actions: BTreeMap<ActionId, ActionDef>,
}

impl ActionRegistry {
    pub fn register(&mut self, def: ActionDef) -> Result<(), String> {
        if self.actions.contains_key(&def.id) {
            return Err(format!("action '{}' registered twice", def.id));
        }
        self.actions.insert(def.id.clone(), def);
        Ok(())
    }

    pub fn get(&self, id: &ActionId) -> Option<&ActionDef> {
        self.actions.get(id)
    }

    pub fn contains(&self, id: &ActionId) -> bool {
        self.actions.contains_key(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActionDef> {
        self.actions.values()
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell`
Expected: 3 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: shell action registry"
```

---

### Task 4: Keystroke and binding parsing

**Files:**
- Modify: `crates/geode-shell/src/lib.rs` (declare module)
- Create: `crates/geode-shell/src/keymap/mod.rs`
- Create: `crates/geode-shell/src/keymap/keystroke.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 6, 7, 8): `Modifiers { ctrl, alt, shift, cmd: bool }` with consts `Modifiers::NONE / CTRL / ALT / CMD` and `union(self, other) -> Modifiers`; `Keystroke { mods: Modifiers, key: String }`; `parse_keystroke(s, mod_alias: Modifiers) -> Result<Keystroke, String>`; `parse_binding(s, mod_alias) -> Result<Vec<Keystroke>, String>` (whitespace-separated sequence, non-empty).

- [ ] **Step 1: Declare modules**

Add to `crates/geode-shell/src/lib.rs` after `pub mod actions;`:
```rust
pub mod keymap;
```

`crates/geode-shell/src/keymap/mod.rs`:
```rust
//! Keymap engine (spec §3.4): keystroke parsing, context predicates,
//! layered binding resolution, and the sequence-aware matcher.
//! Pure logic — no gpui.

mod keystroke;

pub use keystroke::{parse_binding, parse_keystroke, Keystroke, Modifiers};
```

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/keymap/keystroke.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let ks = parse_keystroke("ctrl+shift+h", Modifiers::NONE).unwrap();
        assert!(ks.mods.ctrl && ks.mods.shift && !ks.mods.alt && !ks.mods.cmd);
        assert_eq!(ks.key, "h");
    }

    #[test]
    fn mod_alias_expands() {
        let ks = parse_keystroke("mod+h", Modifiers::ALT).unwrap();
        assert_eq!(ks.mods, Modifiers::ALT);
        let ks = parse_keystroke("mod+shift+q", Modifiers::ALT).unwrap();
        assert!(ks.mods.alt && ks.mods.shift);
    }

    #[test]
    fn keys_are_stored_lowercase() {
        let ks = parse_keystroke("Ctrl+G", Modifiers::NONE).unwrap();
        assert_eq!(ks.key, "g");
        assert!(!ks.mods.shift, "shift is always explicit, never inferred from case");
    }

    #[test]
    fn named_keys_and_digits() {
        assert_eq!(parse_keystroke("ctrl+1", Modifiers::NONE).unwrap().key, "1");
        assert_eq!(parse_keystroke("escape", Modifiers::NONE).unwrap().key, "escape");
    }

    #[test]
    fn binding_sequences_split_on_whitespace() {
        let seq = parse_binding("g g", Modifiers::NONE).unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0].key, "g");
        let seq = parse_binding("mod+h", Modifiers::ALT).unwrap();
        assert_eq!(seq.len(), 1);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_keystroke("", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+shift", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+a+b", Modifiers::NONE).is_err());
        assert!(parse_binding("   ", Modifiers::NONE).is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell keystroke`
Expected: compile error — items not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `keystroke.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers { ctrl: false, alt: false, shift: false, cmd: false };
    pub const CTRL: Modifiers = Modifiers { ctrl: true, alt: false, shift: false, cmd: false };
    pub const ALT: Modifiers = Modifiers { ctrl: false, alt: true, shift: false, cmd: false };
    pub const CMD: Modifiers = Modifiers { ctrl: false, alt: false, shift: false, cmd: true };

    pub fn union(self, other: Modifiers) -> Modifiers {
        Modifiers {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            cmd: self.cmd || other.cmd,
        }
    }
}

/// One key press with modifiers. `key` is lowercase; shift is always an
/// explicit modifier (`shift+g`), never inferred from case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keystroke {
    pub mods: Modifiers,
    pub key: String,
}

/// Parse one keystroke spec like `mod+shift+h`. `mod` expands to
/// `mod_alias` (the user-configurable primary modifier).
pub fn parse_keystroke(s: &str, mod_alias: Modifiers) -> Result<Keystroke, String> {
    let mut mods = Modifiers::NONE;
    let mut key: Option<String> = None;
    if s.is_empty() {
        return Err("empty keystroke".to_string());
    }
    for part in s.split('+') {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "" => return Err(format!("empty segment in '{s}'")),
            "ctrl" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "cmd" | "super" | "win" => mods.cmd = true,
            "mod" => mods = mods.union(mod_alias),
            _ => {
                if key.is_some() {
                    return Err(format!("more than one key in '{s}'"));
                }
                key = Some(lower);
            }
        }
    }
    key.map(|key| Keystroke { mods, key })
        .ok_or_else(|| format!("no key in '{s}'"))
}

/// Parse a binding spec: a whitespace-separated sequence of keystrokes
/// (`"g g"`, `"mod+h"`).
pub fn parse_binding(s: &str, mod_alias: Modifiers) -> Result<Vec<Keystroke>, String> {
    let seq: Result<Vec<_>, _> = s
        .split_whitespace()
        .map(|part| parse_keystroke(part, mod_alias))
        .collect();
    let seq = seq?;
    if seq.is_empty() {
        return Err("empty binding".to_string());
    }
    Ok(seq)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell keystroke`
Expected: 6 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: keystroke and binding-sequence parsing with mod alias"
```

---

### Task 5: Context predicates

**Files:**
- Modify: `crates/geode-shell/src/keymap/mod.rs` (declare module)
- Create: `crates/geode-shell/src/keymap/context.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 6, 7):
  - `KeyContext` with `KeyContext::new(name) -> Self` (name becomes a flag), builder methods `flag(self, f) -> Self` and `pair(self, k, v) -> Self`, plus `has_flag(&str) -> bool`, `get(&str) -> Option<&str>`
  - `Predicate` (Flag / Eq / NotEq / Not / And / Or)
  - `parse_predicate(&str) -> Result<Predicate, String>` — grammar: `or := and ('||' and)*`, `and := unary ('&&' unary)*`, `unary := '!' unary | primary`, `primary := ident | ident '==' value | ident '!=' value | '(' or ')'`; values are bare idents or quoted strings
  - `Predicate::eval(&self, stack: &[KeyContext]) -> bool` — Flag: any context in the stack has it; Eq/NotEq: innermost context defining the key wins, and NotEq is true only when the key IS defined and differs

- [ ] **Step 1: Declare the module**

In `keymap/mod.rs` add:
```rust
mod context;

pub use context::{parse_predicate, KeyContext, Predicate};
```

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/keymap/context.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Vec<KeyContext> {
        vec![
            KeyContext::new("workspace"),
            KeyContext::new("blotter").pair("mode", "normal"),
        ]
    }

    #[test]
    fn flag_matches_any_context_in_stack() {
        let p = parse_predicate("workspace").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("palette").unwrap();
        assert!(!p.eval(&stack()));
    }

    #[test]
    fn eq_uses_innermost_definition() {
        let outer_and_inner = vec![
            KeyContext::new("a").pair("mode", "visual"),
            KeyContext::new("b").pair("mode", "normal"),
        ];
        assert!(parse_predicate("mode == normal").unwrap().eval(&outer_and_inner));
        assert!(!parse_predicate("mode == visual").unwrap().eval(&outer_and_inner));
    }

    #[test]
    fn neq_requires_key_present() {
        assert!(parse_predicate("mode != visual").unwrap().eval(&stack()));
        assert!(!parse_predicate("missing != anything").unwrap().eval(&stack()));
    }

    #[test]
    fn boolean_operators_and_precedence() {
        // ! binds tighter than &&, which binds tighter than ||.
        let p = parse_predicate("palette || blotter && mode == normal").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("!palette && blotter").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("!(blotter && mode == normal)").unwrap();
        assert!(!p.eval(&stack()));
    }

    #[test]
    fn quoted_values() {
        let ctx = vec![KeyContext::new("x").pair("mode", "insert mode")];
        assert!(parse_predicate("mode == \"insert mode\"").unwrap().eval(&ctx));
        assert!(parse_predicate("mode == 'insert mode'").unwrap().eval(&ctx));
    }

    #[test]
    fn parse_errors() {
        assert!(parse_predicate("").is_err());
        assert!(parse_predicate("a &&").is_err());
        assert!(parse_predicate("a == ").is_err());
        assert!(parse_predicate("(a").is_err());
        assert!(parse_predicate("a b").is_err());
        assert!(parse_predicate("mode == \"unterminated").is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell context`
Expected: compile error — items not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `context.rs`:
```rust
/// One frame of the focus-context stack, e.g. `blotter` with `mode=normal`.
/// The stack runs outermost → innermost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyContext {
    flags: Vec<String>,
    pairs: Vec<(String, String)>,
}

impl KeyContext {
    pub fn new(name: impl Into<String>) -> Self {
        KeyContext { flags: vec![name.into()], pairs: Vec::new() }
    }

    pub fn flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.push(flag.into());
        self
    }

    pub fn pair(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.pairs.push((key.into(), value.into()));
        self
    }

    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    Flag(String),
    Eq(String, String),
    NotEq(String, String),
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}

impl Predicate {
    pub fn eval(&self, stack: &[KeyContext]) -> bool {
        match self {
            Predicate::Flag(f) => stack.iter().any(|c| c.has_flag(f)),
            Predicate::Eq(k, v) => lookup(stack, k).is_some_and(|x| x == v),
            Predicate::NotEq(k, v) => lookup(stack, k).is_some_and(|x| x != v),
            Predicate::Not(p) => !p.eval(stack),
            Predicate::And(a, b) => a.eval(stack) && b.eval(stack),
            Predicate::Or(a, b) => a.eval(stack) || b.eval(stack),
        }
    }
}

/// Innermost definition of `key` wins.
fn lookup<'a>(stack: &'a [KeyContext], key: &str) -> Option<&'a str> {
    stack.iter().rev().find_map(|c| c.get(key))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Str(String),
    And,
    Or,
    Not,
    Eq,
    Ne,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '&' => {
                chars.next();
                if chars.next() != Some('&') {
                    return Err("expected '&&'".to_string());
                }
                toks.push(Tok::And);
            }
            '|' => {
                chars.next();
                if chars.next() != Some('|') {
                    return Err("expected '||'".to_string());
                }
                toks.push(Tok::Or);
            }
            '=' => {
                chars.next();
                if chars.next() != Some('=') {
                    return Err("expected '=='".to_string());
                }
                toks.push(Tok::Eq);
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Tok::Ne);
                } else {
                    toks.push(Tok::Not);
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some(ch) if ch == quote => break,
                        Some(ch) => value.push(ch),
                        None => return Err("unterminated string".to_string()),
                    }
                }
                toks.push(Tok::Str(value));
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => {
                let mut ident = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                        ident.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Ident(ident));
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn advance(&mut self) -> Option<Tok> {
        let tok = self.toks.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn parse_or(&mut self) -> Result<Predicate, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Tok::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Predicate::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Predicate, String> {
        let mut left = self.parse_unary()?;
        while self.peek() == Some(&Tok::And) {
            self.advance();
            let right = self.parse_unary()?;
            left = Predicate::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Predicate, String> {
        if self.peek() == Some(&Tok::Not) {
            self.advance();
            Ok(Predicate::Not(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Predicate, String> {
        match self.advance() {
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                if self.advance() != Some(Tok::RParen) {
                    return Err("expected ')'".to_string());
                }
                Ok(inner)
            }
            Some(Tok::Ident(name)) => match self.peek() {
                Some(Tok::Eq) | Some(Tok::Ne) => {
                    let negated = self.advance() == Some(Tok::Ne);
                    let value = match self.advance() {
                        Some(Tok::Ident(v)) | Some(Tok::Str(v)) => v,
                        _ => return Err("expected value after comparison".to_string()),
                    };
                    Ok(if negated {
                        Predicate::NotEq(name, value)
                    } else {
                        Predicate::Eq(name, value)
                    })
                }
                _ => Ok(Predicate::Flag(name)),
            },
            other => Err(format!("unexpected token: {other:?}")),
        }
    }
}

/// Parse a context expression like `blotter && mode == normal`.
pub fn parse_predicate(s: &str) -> Result<Predicate, String> {
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return Err("empty context expression".to_string());
    }
    let mut parser = Parser { toks, pos: 0 };
    let pred = parser.parse_or()?;
    if parser.pos != parser.toks.len() {
        return Err("unexpected trailing tokens".to_string());
    }
    Ok(pred)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell context`
Expected: 6 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: keymap context predicates (parser and evaluator)"
```

---

### Task 6: Building the keymap from layered config docs

**Files:**
- Modify: `crates/geode-shell/src/keymap/mod.rs` (declare module)
- Modify: `crates/geode-shell/Cargo.toml` (dev-dependency)
- Create: `crates/geode-shell/src/keymap/build.rs`

**Interfaces:**
- Consumes: `LayerDoc`, `Diagnostic`, `Severity`, `Layer` from `geode_core::config`; `ActionId`, `ActionRegistry` from `crate::actions`; Task 4 parsing; Task 5 predicates.
- Produces (used by Tasks 7, 8):
  - `UNBOUND_ACTION: &str = "none"`
  - `Binding { keystrokes: Vec<Keystroke>, predicate: Option<Predicate>, action: ActionId, layer: Layer, index: usize }`
  - `Keymap` with `bindings() -> &[Binding]`
  - `build_keymap(layered: &[LayerDoc], mod_alias: Modifiers, registry: &ActionRegistry) -> (Keymap, Vec<Diagnostic>)`

The keymap TOML document format (doc name `keymap`):
```toml
config_version = 1

[[bindings]]
context = "workspace"          # optional; omitted = always active
[bindings.keys]
"mod+h" = "workspace::focus_left"
"g g"   = "table::first_row"
```

- [ ] **Step 1: Declare module and add dev-dependency**

In `keymap/mod.rs` add:
```rust
mod build;

pub use build::{build_keymap, Binding, Keymap, UNBOUND_ACTION};
```

Run: `cargo add --dev tempfile -p geode-shell` (used by Task 8's integration test; added here so the crate manifest changes once).

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/keymap/build.rs`:
```rust
use super::{parse_binding, parse_predicate, Keystroke, Modifiers, Predicate};
use crate::actions::{ActionId, ActionRegistry};
use geode_core::config::{Diagnostic, Layer, LayerDoc, Severity};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionDef;

    fn registry() -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for id in ["workspace::focus_left", "workspace::focus_right"] {
            reg.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
                category: "Test".to_string(),
            })
            .unwrap();
        }
        reg
    }

    fn doc(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: "keymap".to_string(),
            file: format!("{}/keymap.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    #[test]
    fn collects_bindings_in_layer_then_document_order() {
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let bindings = keymap.bindings();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].layer, Layer::Builtin);
        assert!(bindings[0].predicate.is_some());
        assert_eq!(bindings[1].layer, Layer::User);
        assert!(bindings[1].predicate.is_none());
        assert!(bindings[0].index < bindings[1].index);
    }

    #[test]
    fn unknown_action_is_warning_and_skipped() {
        let d = doc(Layer::User, "[[bindings]]\n[bindings.keys]\n\"mod+x\" = \"nope::nothing\"\n");
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("nope::nothing"));
    }

    #[test]
    fn none_action_is_accepted_without_registration() {
        let d = doc(Layer::User, "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n");
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(keymap.bindings()[0].action.0, "none");
    }

    #[test]
    fn bad_context_is_error_and_entry_skipped() {
        let d = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"a &&\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn bad_keystroke_is_error_and_that_key_skipped() {
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+\" = \"workspace::focus_left\"\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert_eq!(keymap.bindings().len(), 1);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn missing_bindings_array_is_fine() {
        let d = doc(Layer::User, "config_version = 1\n");
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert!(diags.is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell build`
Expected: compile error — items not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `build.rs`:
```rust
/// Binding an action to this id unbinds the key: a higher layer silences a
/// lower layer's binding.
pub const UNBOUND_ACTION: &str = "none";

#[derive(Debug, Clone)]
pub struct Binding {
    pub keystrokes: Vec<Keystroke>,
    pub predicate: Option<Predicate>,
    pub action: ActionId,
    pub layer: Layer,
    /// Global definition order across all layers; higher wins ties.
    pub index: usize,
}

#[derive(Debug, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

/// Compile keymap docs (unmerged, in Builtin → Desk → User order) into a
/// flat binding list. Bad entries are skipped with a diagnostic — a typo in
/// a user keymap must never take down the keymap (spec §10.1).
pub fn build_keymap(
    layered: &[LayerDoc],
    mod_alias: Modifiers,
    registry: &ActionRegistry,
) -> (Keymap, Vec<Diagnostic>) {
    let mut bindings = Vec::new();
    let mut diags = Vec::new();
    let mut index = 0usize;
    for doc in layered {
        let entries = match doc.table.get("bindings") {
            None => continue,
            Some(toml::Value::Array(entries)) => entries,
            Some(_) => {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "'bindings' must be an array of tables ([[bindings]])",
                ));
                continue;
            }
        };
        for entry in entries {
            let Some(entry) = entry.as_table() else {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "each [[bindings]] entry must be a table",
                ));
                continue;
            };
            let predicate = match entry.get("context") {
                None => None,
                Some(toml::Value::String(s)) => match parse_predicate(s) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        diags.push(Diagnostic::error(
                            doc.layer,
                            doc.file.clone(),
                            format!("invalid context '{s}': {e}"),
                        ));
                        continue;
                    }
                },
                Some(_) => {
                    diags.push(Diagnostic::error(
                        doc.layer,
                        doc.file.clone(),
                        "'context' must be a string",
                    ));
                    continue;
                }
            };
            let Some(toml::Value::Table(keys)) = entry.get("keys") else {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "[[bindings]] entry is missing its [bindings.keys] table",
                ));
                continue;
            };
            for (spec, action_value) in keys {
                let Some(action_str) = action_value.as_str() else {
                    diags.push(Diagnostic::error(
                        doc.layer,
                        doc.file.clone(),
                        format!("action for '{spec}' must be a string"),
                    ));
                    continue;
                };
                let keystrokes = match parse_binding(spec, mod_alias) {
                    Ok(k) => k,
                    Err(e) => {
                        diags.push(Diagnostic::error(
                            doc.layer,
                            doc.file.clone(),
                            format!("invalid binding '{spec}': {e}"),
                        ));
                        continue;
                    }
                };
                let action = ActionId(action_str.to_string());
                if action_str != UNBOUND_ACTION && !registry.contains(&action) {
                    diags.push(Diagnostic::warning(
                        doc.layer,
                        doc.file.clone(),
                        format!("'{spec}' bound to unknown action '{action_str}' (binding skipped)"),
                    ));
                    continue;
                }
                bindings.push(Binding {
                    keystrokes,
                    predicate: predicate.clone(),
                    action,
                    layer: doc.layer,
                    index,
                });
                index += 1;
            }
        }
    }
    (Keymap { bindings }, diags)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell build`
Expected: 6 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell Cargo.lock
git commit -m "feat: compile layered keymap docs into bindings with diagnostics"
```

---

### Task 7: The matcher

**Files:**
- Modify: `crates/geode-shell/src/keymap/mod.rs` (declare module)
- Create: `crates/geode-shell/src/keymap/matcher.rs`

**Interfaces:**
- Consumes: `Keymap`, `Binding`, `UNBOUND_ACTION` (Task 6), `Keystroke` (Task 4), `KeyContext` (Task 5), `ActionId` (Task 3).
- Produces (used by Task 8 and Phase 1b's dispatcher):
  - `MatchResult { Matched(ActionId), Pending, NoMatch }`
  - `Matcher` (Default) with `press(&mut self, keymap: &Keymap, ks: Keystroke, stack: &[KeyContext]) -> MatchResult`, `pending(&self) -> &[Keystroke]`, `cancel(&mut self)`

Documented semantics (encode these in the tests):
- A binding is a candidate only if its predicate is absent or evaluates true against the stack.
- Exact match: among candidates whose full sequence equals the pending keystrokes, the **last-defined wins** (bindings are in layer-then-definition order, so User beats Desk beats Builtin).
- An exact match fires immediately even if a longer candidate shares the prefix (binding both `g` and `g g` means `g` fires; document, don't fight it).
- If no exact match but some candidate sequence extends the pending prefix → `Pending`.
- Dead end → `NoMatch`, pending cleared; the terminating keystroke is *not* retried as a fresh start (v1 simplification).
- A winning binding whose action is `none` clears pending and returns `NoMatch` (the unbind swallows the key).

- [ ] **Step 1: Declare the module**

In `keymap/mod.rs` add:
```rust
mod matcher;

pub use matcher::{MatchResult, Matcher};
```

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/keymap/matcher.rs`:
```rust
use super::{KeyContext, Keymap, Keystroke, UNBOUND_ACTION};
use crate::actions::ActionId;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionDef, ActionRegistry};
    use crate::keymap::{build_keymap, parse_keystroke, Modifiers};
    use geode_core::config::{Layer, LayerDoc};

    fn registry(ids: &[&str]) -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for id in ids {
            reg.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
                category: "Test".to_string(),
            })
            .unwrap();
        }
        reg
    }

    fn keymap(docs: &[(Layer, &str)], ids: &[&str]) -> Keymap {
        let docs: Vec<LayerDoc> = docs
            .iter()
            .map(|(layer, text)| LayerDoc {
                layer: *layer,
                name: "keymap".to_string(),
                file: format!("{}/keymap.toml", layer.name()).into(),
                table: text.parse().unwrap(),
            })
            .collect();
        let (keymap, diags) = build_keymap(&docs, Modifiers::ALT, &registry(ids));
        assert!(diags.is_empty(), "{diags:?}");
        keymap
    }

    fn ks(s: &str) -> Keystroke {
        parse_keystroke(s, Modifiers::ALT).unwrap()
    }

    fn ws() -> Vec<KeyContext> {
        vec![KeyContext::new("workspace")]
    }

    #[test]
    fn simple_match() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n")],
            &["a::left"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("mod+h"), &ws()), MatchResult::Matched(ActionId("a::left".into())));
        assert!(m.pending().is_empty());
    }

    #[test]
    fn later_layer_wins() {
        let km = keymap(
            &[
                (Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n"),
                (Layer::User, "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::right\"\n"),
            ],
            &["a::left", "a::right"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("mod+h"), &ws()), MatchResult::Matched(ActionId("a::right".into())));
    }

    #[test]
    fn unbind_swallows_key() {
        let km = keymap(
            &[
                (Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n"),
                (Layer::User, "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n"),
            ],
            &["a::left"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("mod+h"), &ws()), MatchResult::NoMatch);
        assert!(m.pending().is_empty());
    }

    #[test]
    fn sequence_pending_then_match() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n")],
            &["a::top"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        assert_eq!(m.pending().len(), 1);
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Matched(ActionId("a::top".into())));
        assert!(m.pending().is_empty());
    }

    #[test]
    fn sequence_dead_end_clears_without_retry() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n\"x\" = \"a::x\"\n")],
            &["a::top", "a::x"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        // "g x" is a dead end; the "x" is not retried as a fresh start.
        assert_eq!(m.press(&km, ks("x"), &ws()), MatchResult::NoMatch);
        assert!(m.pending().is_empty());
        // But a fresh "x" now matches.
        assert_eq!(m.press(&km, ks("x"), &ws()), MatchResult::Matched(ActionId("a::x".into())));
    }

    #[test]
    fn exact_match_beats_longer_candidate() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"g\" = \"a::g\"\n\"g g\" = \"a::gg\"\n")],
            &["a::g", "a::gg"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Matched(ActionId("a::g".into())));
    }

    #[test]
    fn context_gates_bindings() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"j\" = \"b::down\"\n")],
            &["b::down"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("j"), &ws()), MatchResult::NoMatch);
        let blotter = vec![KeyContext::new("workspace"), KeyContext::new("blotter")];
        assert_eq!(m.press(&km, ks("j"), &blotter), MatchResult::Matched(ActionId("b::down".into())));
    }

    #[test]
    fn cancel_clears_pending() {
        let km = keymap(
            &[(Layer::Builtin, "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n")],
            &["a::top"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        m.cancel();
        assert!(m.pending().is_empty());
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell matcher`
Expected: compile error — `Matcher`, `MatchResult` not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `matcher.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Matched(ActionId),
    /// The keystrokes so far are a prefix of at least one binding; awaiting more.
    Pending,
    NoMatch,
}

/// Sequence-aware key matcher. One per focus target is unnecessary — the
/// shell holds one and feeds it the active context stack per press.
#[derive(Debug, Default)]
pub struct Matcher {
    pending: Vec<Keystroke>,
}

impl Matcher {
    pub fn press(
        &mut self,
        keymap: &Keymap,
        keystroke: Keystroke,
        stack: &[KeyContext],
    ) -> MatchResult {
        self.pending.push(keystroke);
        let mut exact: Option<&super::Binding> = None;
        let mut has_longer_candidate = false;
        for binding in keymap.bindings() {
            if binding
                .predicate
                .as_ref()
                .is_some_and(|p| !p.eval(stack))
            {
                continue;
            }
            if binding.keystrokes == self.pending {
                // Bindings are in layer-then-definition order; keep the last.
                exact = Some(binding);
            } else if binding.keystrokes.len() > self.pending.len()
                && binding.keystrokes.starts_with(&self.pending)
            {
                has_longer_candidate = true;
            }
        }
        if let Some(binding) = exact {
            self.pending.clear();
            if binding.action.0 == UNBOUND_ACTION {
                return MatchResult::NoMatch;
            }
            return MatchResult::Matched(binding.action.clone());
        }
        if has_longer_candidate {
            return MatchResult::Pending;
        }
        self.pending.clear();
        MatchResult::NoMatch
    }

    pub fn pending(&self) -> &[Keystroke] {
        &self.pending
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell matcher`
Expected: 8 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: sequence-aware keymap matcher with layering and unbind"
```

---

### Task 8: Builtin defaults and the full-stack integration test

**Files:**
- Modify: `crates/geode-shell/src/lib.rs` (declare module)
- Create: `crates/geode-shell/src/defaults.rs`
- Create: `crates/geode-shell/tests/keymap_integration.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–7.
- Produces (used by Phase 1b's app wiring):
  - `defaults::BUILTIN_KEYMAP: &str` — the compiled-in keymap doc
  - `defaults::register_builtin_actions(&mut ActionRegistry)` — workspace actions (focus/split/fullscreen/close, switch_1..switch_9) and `palette::toggle`
  - `defaults::default_mod() -> Modifiers` (Alt)
  - `defaults::mod_alias_from_config(&Config) -> Modifiers` — reads doc `app`, path `keymap.mod` (`"alt" | "ctrl" | "cmd"`), falls back to the default

- [ ] **Step 1: Declare the module**

Add to `crates/geode-shell/src/lib.rs`:
```rust
pub mod defaults;
```

- [ ] **Step 2: Write the failing defaults tests**

`crates/geode-shell/src/defaults.rs`:
```rust
use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::Config;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::build_keymap;
    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn builtin_keymap_builds_clean_against_builtin_actions() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(diags.is_empty(), "builtin keymap must be diagnostic-free: {diags:?}");
        assert!(keymap.bindings().len() >= 18);
    }

    #[test]
    fn mod_alias_read_from_config_with_fallback() {
        let config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"ctrl\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(mod_alias_from_config(&config), Modifiers::CTRL);
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(mod_alias_from_config(&empty), default_mod());
        let bogus = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"hyper\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(mod_alias_from_config(&bogus), default_mod());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell defaults`
Expected: compile error — items not defined.

- [ ] **Step 4: Implement defaults**

Insert above the tests in `defaults.rs`:
```rust
//! Compiled-in defaults: the builtin action set and keymap (spec §3.1).
//! These form the Builtin config layer; desk and user files override them.

/// The builtin keymap document, layered under desk/user keymaps.
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+j" = "workspace::focus_down"
"mod+k" = "workspace::focus_up"
"mod+l" = "workspace::focus_right"
"mod+v" = "workspace::split_vertical"
"mod+s" = "workspace::split_horizontal"
"mod+f" = "workspace::fullscreen_tile"
"mod+shift+q" = "workspace::close_tile"
"mod+1" = "workspace::switch_1"
"mod+2" = "workspace::switch_2"
"mod+3" = "workspace::switch_3"
"mod+4" = "workspace::switch_4"
"mod+5" = "workspace::switch_5"
"mod+6" = "workspace::switch_6"
"mod+7" = "workspace::switch_7"
"mod+8" = "workspace::switch_8"
"mod+9" = "workspace::switch_9"

[[bindings]]
[bindings.keys]
"mod+p" = "palette::toggle"
"ctrl+shift+p" = "palette::toggle"
"#;

fn action(reg: &mut ActionRegistry, id: &str, title: &str, category: &str) {
    reg.register(ActionDef {
        id: ActionId(id.to_string()),
        title: title.to_string(),
        category: category.to_string(),
    })
    .expect("builtin action ids are unique by construction");
}

/// Register the shell's own actions. Modules register theirs at module
/// registration time (spec §9.1); these are the shell's.
pub fn register_builtin_actions(reg: &mut ActionRegistry) {
    action(reg, "workspace::focus_left", "Focus left", "Workspace");
    action(reg, "workspace::focus_down", "Focus down", "Workspace");
    action(reg, "workspace::focus_up", "Focus up", "Workspace");
    action(reg, "workspace::focus_right", "Focus right", "Workspace");
    action(reg, "workspace::split_vertical", "Split vertical", "Workspace");
    action(reg, "workspace::split_horizontal", "Split horizontal", "Workspace");
    action(reg, "workspace::fullscreen_tile", "Fullscreen tile", "Workspace");
    action(reg, "workspace::close_tile", "Close tile", "Workspace");
    for i in 1..=9 {
        action(
            reg,
            &format!("workspace::switch_{i}"),
            &format!("Switch to workspace {i}"),
            "Workspace",
        );
    }
    action(reg, "palette::toggle", "Toggle command palette", "Palette");
}

/// The default primary modifier (spec §3.1: Alt, remappable).
pub fn default_mod() -> Modifiers {
    Modifiers::ALT
}

/// Resolve the `mod` alias from config: doc `app`, key `keymap.mod`.
pub fn mod_alias_from_config(config: &Config) -> Modifiers {
    match config.get("app", "keymap.mod").and_then(|v| v.as_str()) {
        Some("ctrl") => Modifiers::CTRL,
        Some("cmd") => Modifiers::CMD,
        Some("alt") => Modifiers::ALT,
        _ => default_mod(),
    }
}
```

- [ ] **Step 5: Run defaults tests to verify they pass**

Run: `cargo test -p geode-shell defaults`
Expected: 2 tests PASS.

- [ ] **Step 6: Write the full-stack integration test**

`crates/geode-shell/tests/keymap_integration.rs`:
```rust
//! Full 1a stack: config directories → layered docs → keymap → matcher.
//! Mirrors the real wiring Phase 1b will do in the app.

use geode_core::config::{Config, ConfigSources, LayerDoc};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::defaults;
use geode_shell::keymap::{build_keymap, parse_keystroke, KeyContext, MatchResult, Matcher};

#[test]
fn desk_overrides_user_unbinds_and_sequences_work() {
    let desk = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    // Desk rebinds mod+h; adds a sequence binding.
    std::fs::write(
        desk.path().join("keymap.toml"),
        "config_version = 1\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::split_vertical\"\n\"g g\" = \"workspace::focus_up\"\n",
    )
    .unwrap();
    // User unbinds fullscreen. (Mod remapping is covered by defaults' unit
    // tests; the default Alt alias is used here.)
    std::fs::write(
        user.path().join("keymap.toml"),
        "config_version = 1\n\n[[bindings]]\n[bindings.keys]\n\"mod+f\" = \"none\"\n",
    )
    .unwrap();

    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", defaults::BUILTIN_KEYMAP).unwrap()],
        desk: Some(desk.path().to_path_buf()),
        user: Some(user.path().to_path_buf()),
    });
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

    let mut registry = ActionRegistry::default();
    defaults::register_builtin_actions(&mut registry);
    let mod_alias = defaults::mod_alias_from_config(&config);
    let (keymap, diags) = build_keymap(config.layered_docs("keymap"), mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");

    let stack = vec![KeyContext::new("workspace")];
    let mut matcher = Matcher::default();
    let ks = |s: &str| parse_keystroke(s, mod_alias).unwrap();

    // Desk override beats builtin.
    assert_eq!(
        matcher.press(&keymap, ks("mod+h"), &stack),
        MatchResult::Matched(ActionId("workspace::split_vertical".into()))
    );
    // User unbind swallows the builtin binding.
    assert_eq!(matcher.press(&keymap, ks("mod+f"), &stack), MatchResult::NoMatch);
    // Untouched builtin binding still works.
    assert_eq!(
        matcher.press(&keymap, ks("mod+j"), &stack),
        MatchResult::Matched(ActionId("workspace::focus_down".into()))
    );
    // Desk-added sequence: pending, then match.
    assert_eq!(matcher.press(&keymap, ks("g"), &stack), MatchResult::Pending);
    assert_eq!(
        matcher.press(&keymap, ks("g"), &stack),
        MatchResult::Matched(ActionId("workspace::focus_up".into()))
    );
}
```

- [ ] **Step 7: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: all tests pass — 13 in geode-core, 25 in geode-shell unit tests, 1 integration, 5 in geode-demo-data.

- [ ] **Step 8: Lint, format, commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`

```bash
git add crates/geode-shell
git commit -m "feat: builtin shell defaults and full-stack keymap integration test"
```

---

## Self-Review Notes

- **Spec coverage:** §8 config layering (Tasks 1–2: layers, merge-by-key with user-wins, whole-object override for named objects, provenance/explain, versioning, degrade-not-crash), §3.4 keymap engine (Tasks 4–7: one declarative format, layered built-in→desk→user, contexts, chords and multi-key sequences, modules-expose-actions via Task 3's registry), §3.1 default bindings and remappable mod (Task 8). Hot reload (spec §8) is Phase 1b — it needs the runtime. The `explain` UI and config editor are later phases; the data (`Config::explain`) exists now.
- **Type consistency:** `LayerDoc`/`Diagnostic`/`Layer` shapes match across Tasks 1, 2, 6; `Keystroke`/`Modifiers` across 4, 6, 7, 8; `build_keymap(&[LayerDoc], Modifiers, &ActionRegistry)` signature identical in Tasks 6, 7 (tests), 8.
- **Test-count expectations** in Task 8 Step 7 assume no extra tests are added; treat the named counts as minimums if an implementer adds cases.
