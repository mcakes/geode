use super::{CONFIG_VERSION, Diagnostic, Layer, LayerDoc};
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
}
