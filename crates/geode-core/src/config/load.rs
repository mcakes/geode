use super::{CONFIG_VERSION, Config, Diagnostic, Layer, LayerDoc, Severity};
use crate::schema::SchemaSpec;
use crate::view::{Colour, DatasetPresentationSpec, ViewPresentationSpec, ViewSpec};
use std::path::Path;

/// Read `*.toml` files in `root`, non-recursively and in sorted path order.
/// Missing or unreadable directories and failed directory entries are skipped.
/// File read, TOML parse, and unsupported-version errors diagnose and skip the
/// file; a missing version warns and assumes [`CONFIG_VERSION`].
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

/// Read merged view definitions and apply presentation overlays per property:
/// kind default → view definition → dataset presentation → view presentation.
/// A missing `views` document returns no views or diagnostics.
///
/// Named colours are checked at each definition site before overlays can hide
/// invalid values or reorder columns. Definition diagnostics use file column
/// indices; overlay diagnostics use column names. Dataset colours are checked
/// even when no view-presentation document exists.
///
/// The raw merged `views` document stays unchanged so dialogs can persist view
/// definitions separately from presentation. Returned diagnostics cover views,
/// overlay parsing/application, and colour references; schema and named-colour
/// definition diagnostics are left to their readers' callers.
pub fn load_views(config: &Config) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
    let Some(views_doc) = config.doc("views") else {
        return (Vec::new(), Vec::new());
    };
    let (mut views, mut diags) = ViewSpec::from_doc(views_doc);

    // Resolve dataset presentation after view definitions and before the
    // view-specific overlay.
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    let dataset_overlay = config
        .doc(crate::view::DATASET_PRESENTATION_DOC)
        .map(|doc| {
            let (spec, d) = DatasetPresentationSpec::from_doc(doc);
            diags.extend(d);
            spec
        });
    let colours = config
        .doc("colours")
        .map(|d| crate::colour::NamedColours::from_doc(d).0)
        .unwrap_or_default();

    // Dataset colour references must be checked even without a
    // `view_presentation` document.
    if let Some(overlay) = &dataset_overlay {
        for (dataset, columns) in &overlay.datasets {
            for (col, cp) in columns {
                let Some(Colour::Named(name)) = &cp.colour else {
                    continue;
                };
                if colours.get(name).is_none() {
                    diags.push(Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        message: format!(
                            "dataset presentation '{dataset}': column '{col}' names colour '{name}', which colours.toml does not define — painted in foreground"
                        ),
                        path: Some(format!(
                            "dataset_presentation.{dataset}.columns.{col}.colour"
                        )),
                    });
                }
            }
        }
    }

    for view in &views {
        for (i, column) in view.columns.iter().enumerate() {
            if let Some(Colour::Named(name)) = &view.presentation_of(column.name()).colour
                && colours.get(name).is_none()
            {
                diags.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: format!(
                        "view '{}': column '{}' names colour '{name}', which colours.toml does not define — painted in foreground",
                        view.name,
                        column.name()
                    ),
                    path: Some(format!("views.{}.columns.{i}.format.colour", view.name)),
                });
            }
        }
    }

    // Validate definition colours before applying this overlay: otherwise a
    // valid override could hide an invalid definition, or an overlay's error
    // could be attributed to a definition that never named that colour.
    if let Some(overlay) = &dataset_overlay {
        diags.extend(overlay.apply(&mut views, &schema));
    }

    if let Some(doc) = config.doc("view_presentation") {
        let (presentation, d) = ViewPresentationSpec::from_doc(doc);
        diags.extend(d);

        for (view_name, p) in &presentation.views {
            for (col, cp) in &p.columns {
                let Some(Colour::Named(name)) = &cp.colour else {
                    continue;
                };
                if colours.get(name).is_none() {
                    diags.push(Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        message: format!(
                            "view presentation '{view_name}': column '{col}' names colour '{name}', which colours.toml does not define — painted in foreground"
                        ),
                        path: Some(format!(
                            "view_presentation.{view_name}.columns.{col}.colour"
                        )),
                    });
                }
            }
        }
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

    /// A user view replaces the desk definition as a whole; presentation then
    /// reorders and hides columns in that effective definition.
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

    #[test]
    fn a_column_naming_an_unknown_colour_warns_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "views.toml",
            "config_version = 1\n[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"d\"\nformat = { colour = \"ghost\" }\n",
        );
        write(
            dir.path(),
            "colours.toml",
            "config_version = 1\n[delta]\nhue = 240\n",
        );
        let config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: Some(dir.path().to_path_buf()),
            user: None,
        });
        let (_views, diags) = load_views(&config);
        assert!(
            diags.iter().any(
                |d| d.path.as_deref() == Some("views.tree.columns.1.format.colour")
                    && d.message.contains("ghost")
            ),
            "{diags:?}"
        );
    }

    #[test]
    fn a_presentation_naming_an_unknown_colour_warns_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "views.toml",
            "config_version = 1\n[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
        );
        write(
            dir.path(),
            "colours.toml",
            "config_version = 1\n[delta]\nhue = 240\n",
        );
        write(
            dir.path(),
            "view_presentation.toml",
            "config_version = 1\n[tree.columns.npv]\ncolour = \"ghost\"\n",
        );
        let config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: Some(dir.path().to_path_buf()),
            user: None,
        });
        let (_views, diags) = load_views(&config);
        assert!(
            diags.iter().any(|d| d.path.as_deref()
                == Some("view_presentation.tree.columns.npv.colour")
                && d.message.contains("ghost")),
            "{diags:?}"
        );
    }

    #[test]
    fn load_views_merges_the_dataset_overlay_under_the_view_overlay_and_reports_its_colours() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\nwidth = 50\n[w]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[w.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[risk.columns.npv]\nwidth = 140\ncolour = \"nope\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("view_presentation", "[v.columns.npv]\nwidth = 200\n").unwrap(),
                LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let (views, diags) = load_views(&config);
        let v = views.iter().find(|v| v.name == "v").unwrap();
        let w = views.iter().find(|v| v.name == "w").unwrap();
        assert_eq!(
            v.presentation_of("npv").width,
            Some(200.0),
            "view overlay wins in v"
        );
        assert_eq!(
            w.presentation_of("npv").width,
            Some(140.0),
            "dataset level reaches w"
        );
        assert_eq!(
            w.presentation_of("npv").colour,
            Some(Colour::Named("nope".into())),
            "an unknown colour still merges; it is warned about, not dropped"
        );
        let colour_paths: Vec<&str> = diags
            .iter()
            .filter_map(|d| d.path.as_deref())
            .filter(|p| p.ends_with(".colour"))
            .collect();
        assert_eq!(
            colour_paths,
            vec!["dataset_presentation.risk.columns.npv.colour"],
            "one mistake, one diagnostic: the dataset-level colour must not \
             also be reported at `views.<v>.columns.<i>.format.colour`, a \
             path into a file that holds no colour key at all"
        );
        let colour_warning = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("dataset_presentation.risk.columns.npv.colour"))
            .expect("the dataset overlay's colour is cross-checked");
        assert!(
            colour_warning.message.contains("nope"),
            "{}",
            colour_warning.message
        );
    }

    /// A valid dataset colour override must not hide a warning about the
    /// view definition's own invalid colour.
    #[test]
    fn the_desk_views_own_colour_check_reads_the_desk_value() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\nformat = { colour = \"ghost\" }\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[risk.columns.npv]\ncolour = \"delta\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let (views, diags) = load_views(&config);
        let v = views.iter().find(|v| v.name == "v").unwrap();
        assert_eq!(
            v.presentation_of("npv").colour,
            Some(Colour::Named("delta".into())),
            "the dataset level still wins the resolution; only the \
             cross-check reads the desk's own key"
        );
        let colour_paths: Vec<&str> = diags
            .iter()
            .filter_map(|d| d.path.as_deref())
            .filter(|p| p.ends_with(".colour"))
            .collect();
        assert_eq!(
            colour_paths,
            vec!["views.v.columns.1.format.colour"],
            "the desk's own unknown colour is still warned about, at the \
             views path, even under a valid dataset-level colour"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("ghost")),
            "the diagnostic names the desk's own colour: {diags:?}"
        );
    }

    #[test]
    fn a_dataset_overlay_colour_is_cross_checked_without_a_view_overlay() {
        // Omit `view_presentation` to verify that dataset colour warnings do
        // not depend on its presence.
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[risk.columns.npv]\ncolour = \"nope\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let (views, diags) = load_views(&config);
        let v = views.iter().find(|v| v.name == "v").unwrap();
        assert_eq!(
            v.presentation_of("npv").colour,
            Some(Colour::Named("nope".into())),
            "an unknown colour still merges even with no view_presentation doc"
        );
        let colour_warning = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("dataset_presentation.risk.columns.npv.colour"))
            .expect(
                "the dataset overlay's colour is cross-checked without a view_presentation doc",
            );
        assert!(
            colour_warning.message.contains("nope"),
            "{}",
            colour_warning.message
        );
    }
}
