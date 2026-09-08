use super::{CONFIG_VERSION, Config, Diagnostic, Layer, LayerDoc};
use crate::view::{ViewPresentationSpec, ViewSpec};
use std::path::Path;

#[cfg(test)]
use super::Severity;

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
                    format!(
                        "unsupported config_version {other} (this build supports {CONFIG_VERSION})"
                    ),
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
        docs.push(LayerDoc {
            layer,
            name,
            file: path,
            table,
        });
    }
    (docs, diags)
}

/// The views a module is handed: the merged `views` doc read into
/// `ViewSpec`s, with `view_presentation` merged **over** them.
///
/// This is the one door to a `ViewSpec`, and the ordering is the whole
/// point. `Config::load` has already done the named-object merge, so the
/// `views` doc here is the desk's view with any user override applied
/// whole-object (`config::merge::atomic_depth`). Only then does
/// presentation reorder, hide and resize within it — so a user-layer
/// view override and a personal presentation file compose rather than
/// race, and no module can ever observe a view the trader's presentation
/// has not yet touched.
///
/// It cannot be folded into `Config::load` itself by rewriting the
/// merged `views` table, tempting as that would be for
/// unbypassability: the Views dialog reads that same doc to decide what
/// is definitional (`Destination::Doc`) and what is presentation
/// (`Destination::Presentation`), and a doc with presentation already
/// folded in would make it write widths back into `views.toml` — exactly
/// the fork this design exists to prevent (spec §4.1).
///
/// Diagnostics are the readers' own plus the merge's mismatch warnings.
/// A missing `views` doc is an empty list, not an error: callers that
/// need to distinguish "no views configured" ask `Config::doc` first.
pub fn load_views(config: &Config) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
    let Some(views_doc) = config.doc("views") else {
        return (Vec::new(), Vec::new());
    };
    let (mut views, mut diags) = ViewSpec::from_doc(views_doc);
    if let Some(doc) = config.doc("view_presentation") {
        let (presentation, d) = ViewPresentationSpec::from_doc(doc);
        diags.extend(d);
        diags.extend(presentation.apply(&mut views));
    }
    (views, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigSources};

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn loads_docs_by_file_stem_sorted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "keymap.toml", "config_version = 1\n");
        write(
            dir.path(),
            "app.toml",
            "config_version = 1\n[keymap]\nmod = \"alt\"\n",
        );
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

    /// The merge runs AFTER the named-object merge, so a user-layer view
    /// override and a presentation file compose rather than race: the
    /// desk's `tree` is merged whole-object first, and only then does the
    /// user's presentation reorder and hide within it. Order and hidden
    /// are both asserted through the returned `ViewSpec`, which is the
    /// only thing a module is ever handed.
    #[test]
    fn presentation_is_merged_over_the_view_after_the_named_object_merge() {
        let desk = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write(
            desk.path(),
            "views.toml",
            "config_version = 1\n\
             [tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n\
             [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        );
        write(
            user.path(),
            "view_presentation.toml",
            "config_version = 1\n[tree]\norder = [\"npv\", \"book\"]\nhidden = [\"delta01\"]\n",
        );
        let config = Config::load(&ConfigSources {
            builtin: Vec::new(),
            desk: Some(desk.path().to_path_buf()),
            user: Some(user.path().to_path_buf()),
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

        let (views, diags) = load_views(&config);
        assert!(diags.is_empty(), "{diags:?}");
        let tree = views.iter().find(|v| v.name == "tree").expect("tree");
        let names: Vec<&str> = tree.columns.iter().map(|c| c.name()).collect();
        assert_eq!(
            names,
            vec!["npv", "book", "delta01"],
            "named columns lead in the presentation's order; the rest keep file order"
        );
        assert_eq!(tree.presentation_of("delta01").hidden, Some(true));
        assert_eq!(
            config.explain("views", "tree"),
            Some(Layer::Desk),
            "presentation is a separate doc: it must not fork the view's own layer"
        );
    }
}
