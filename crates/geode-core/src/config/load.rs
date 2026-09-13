use super::{CONFIG_VERSION, Config, Diagnostic, Layer, LayerDoc, Severity};
use crate::schema::SchemaSpec;
use crate::view::{Colour, DatasetPresentationSpec, ViewPresentationSpec, ViewSpec};
use std::path::Path;

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
///
/// Cross-checks a column's named colour (Part 2c §3–§4; dataset overlay
/// spec §2.3) against the `colours` doc at each of the three places a
/// colour can be named, since no reader has access to the `colours` doc
/// to check it itself. The view's own `format.colour` is checked right
/// here, between the two reads above: the index in the diagnostic's
/// path must be the FILE's own column order, which is what `views`
/// still is at this point — the overlays' `apply` calls reorder, hide
/// and resize it. The dataset overlay's `[dataset.columns.<col>].colour`
/// is checked right after `colours` is bound, UNCONDITIONALLY on
/// `dataset_overlay` alone — never nested inside the `view_presentation`
/// block below, since a desk with no `view_presentation.toml` at all
/// (the default state) must still hear about it (spec §2.3; the whole-
/// branch review's Critical). The view overlay's own
/// `[view.columns.<col>].colour` (Part 2c §4) is checked just below,
/// against `presentation.views` directly rather than the merged result
/// — it is keyed by column name, not file position, so it needs no such
/// ordering care.
pub fn load_views(config: &Config) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
    let Some(views_doc) = config.doc("views") else {
        return (Vec::new(), Vec::new());
    };
    let (mut views, mut diags) = ViewSpec::from_doc(views_doc);

    // The dataset-level overlay merges BETWEEN the desk's own keys
    // (already in `presentation` from `from_doc`) and the view overlay
    // below, so the resolved order per key is kind default → desk view
    // column → dataset-level → view-level (dataset-presentation spec §3.1).
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
    if let Some(overlay) = &dataset_overlay {
        diags.extend(overlay.apply(&mut views, &schema));
    }

    let colours = config
        .doc("colours")
        .map(|d| crate::colour::NamedColours::from_doc(d).0)
        .unwrap_or_default();

    // Unconditional on `dataset_overlay` alone — NOT nested inside the
    // `view_presentation` block below, since a desk with no
    // view_presentation.toml at all (the default state) must still hear
    // about an unknown colour named at the dataset level (spec §2.3).
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

    #[test]
    fn a_dataset_overlay_colour_is_cross_checked_without_a_view_overlay() {
        // No `view_presentation` doc at all — the default state for a
        // desk that has never opened a presentation dialog. The
        // dataset-overlay colour cross-check must not depend on that
        // doc's presence (spec §2.3; the whole-branch review's Critical).
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
