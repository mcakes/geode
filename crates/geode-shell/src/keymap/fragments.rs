//! Default bindings contributed by module factories.
//!
//! The app inserts fragments after shell builtin keymaps and before desk/user
//! keymaps. Each fragment reports [`Layer::Builtin`] and a synthetic module file
//! name, so ordinary keymap precedence, diagnostics, and reset behavior apply.
//!
//! [`check_fragment`] restricts context spellings to a module's declared context
//! names and rejects negation, disjunction, and parentheses. Modules should use a
//! bare owning context followed by optional conjunctions, such as
//! `blotter && mode == normal`. This keeps ordinary module defaults scoped to the
//! tile that owns them without adding module dependencies to the shell.

use geode_core::config::{Diagnostic, Layer, LayerDoc};
use std::path::PathBuf;

/// Synthetic diagnostic source for a compiled-in fragment, `<module:kind>`.
/// This identifies the contributing module without implying a disk file exists.
pub fn fragment_file(kind: &str) -> PathBuf {
    PathBuf::from(format!("<module:{kind}>"))
}

/// Parse fragment text as a builtin-layer keymap document.
/// Malformed TOML returns an error diagnostic identifying the module. Context,
/// key, and action validation happens separately.
pub fn fragment_doc(kind: &str, text: &str) -> Result<LayerDoc, Diagnostic> {
    let table = text.parse::<toml::Table>().map_err(|e| {
        Diagnostic::error(
            Layer::Builtin,
            fragment_file(kind),
            format!("module '{kind}' keymap fragment does not parse: {e}"),
        )
    })?;
    Ok(LayerDoc {
        layer: Layer::Builtin,
        name: "keymap".to_string(),
        file: fragment_file(kind),
        table,
    })
}

/// Filter entries by the fragment context spelling restrictions.
///
/// A context must be a string whose first scanned identifier belongs to
/// `contexts`. Any `!`, `||`, or `(` anywhere in the text rejects the entry,
/// including these characters inside quoted values and the `!` in `!=`.
/// Each rejected entry produces an error diagnostic.
///
/// This is a textual check, not a proof that the parsed predicate requires a
/// module flag: it does not verify that the first identifier is used as a flag
/// rather than a comparison key. Authors must use `ctx` or `ctx && ...`.
/// [`super::build_keymap`] subsequently validates accepted predicate syntax,
/// keys, and actions. Non-array `bindings` values are left for that compiler.
pub fn check_fragment(doc: LayerDoc, contexts: &[&str]) -> (LayerDoc, Vec<Diagnostic>) {
    let mut doc = doc;
    let mut diags = Vec::new();
    // Anything other than an array of tables is left exactly as it is:
    // `build_keymap` diagnoses those shapes itself, against this same
    // `file`, and a second opinion here would double-report them.
    let Some(toml::Value::Array(entries)) = doc.table.get("bindings") else {
        return (doc, diags);
    };
    let refuse = |message: String| Diagnostic::error(Layer::Builtin, doc.file.clone(), message);
    let listed = contexts.join(", ");
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry.as_table().and_then(|t| t.get("context")) {
            Some(toml::Value::String(predicate)) => {
                if let Some(token) = non_conjunction_token(predicate) {
                    diags.push(refuse(format!(
                        "keymap fragment binds in context '{predicate}', which uses '{token}': a \
                         fragment predicate must be a plain conjunction ('ctx' or \
                         'ctx && key == value'), so that naming one of this module's own \
                         contexts ({listed}) confines the binding to it — binding dropped"
                    )));
                    continue;
                }
                match first_identifier(predicate) {
                    Some(name) if contexts.contains(&name) => kept.push(entry.clone()),
                    _ => diags.push(refuse(format!(
                        "keymap fragment binds in context '{predicate}', which is not one of \
                         this module's own contexts ({listed}) — binding dropped"
                    ))),
                }
            }
            Some(_) => diags.push(refuse(
                "keymap fragment has a [[bindings]] entry whose 'context' is not a string; a \
                 fragment binding must name one of this module's own contexts — binding dropped"
                    .to_string(),
            )),
            None => diags.push(refuse(format!(
                "keymap fragment has a [[bindings]] entry with no context; a fragment binding \
                 must name one of this module's own contexts ({listed}) — binding dropped"
            ))),
        }
    }
    doc.table
        .insert("bindings".to_string(), toml::Value::Array(kept));
    (doc, diags)
}

/// First occurrence of `!`, `||`, or `(`, including inside quoted values.
/// A stray closing parenthesis is handled by the predicate parser instead.
fn non_conjunction_token(predicate: &str) -> Option<&'static str> {
    let mut found: Option<(usize, &'static str)> = None;
    for token in ["!", "||", "("] {
        if let Some(at) = predicate.find(token)
            && found.is_none_or(|(first, _)| at < first)
        {
            found = Some((at, token));
        }
    }
    found.map(|(_, token)| token)
}

/// Scan for the first ASCII letter or underscore, then consume identifier
/// characters (ASCII alphanumeric, `_`, `-`). The context restriction is based
/// on this source spelling; this helper does not parse a predicate.
fn first_identifier(predicate: &str) -> Option<&str> {
    let start = predicate.find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
    let rest = &predicate[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Insert fragments after every builtin document and before all non-builtin
/// documents. Relative order within each group and within `fragments` is retained.
/// The caller supplies desk/user order; this function does not sort those layers.
pub fn splice(layered: &[LayerDoc], fragments: &[LayerDoc]) -> Vec<LayerDoc> {
    let mut out = Vec::with_capacity(layered.len() + fragments.len());
    out.extend(
        layered
            .iter()
            .filter(|d| d.layer == Layer::Builtin)
            .cloned(),
    );
    out.extend(fragments.iter().cloned());
    out.extend(
        layered
            .iter()
            .filter(|d| d.layer != Layer::Builtin)
            .cloned(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::Severity;

    #[test]
    fn a_fragment_binding_outside_the_modules_contexts_is_dropped_with_a_diagnostic() {
        let doc = fragment_doc(
            "rec",
            "[[bindings]]\ncontext = \"rec && mode == normal\"\n[bindings.keys]\n\"j\" = \"rec::down\"\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+q\" = \"rec::quit\"\n\n[[bindings]]\n[bindings.keys]\n\"x\" = \"rec::x\"\n",
        )
        .unwrap();
        let (kept, diags) = check_fragment(doc, &["rec"]);
        let entries = kept.table["bindings"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(diags[0].message.contains("workspace") && diags[0].message.contains("rec"));
        assert!(diags[1].message.contains("no context"));
        assert_eq!(kept.layer, Layer::Builtin);
        assert_eq!(kept.file.to_string_lossy(), "<module:rec>");
    }

    /// A factory's kind and declared contexts can differ. Only its declared
    /// contexts authorize a fragment entry.
    #[test]
    fn a_fragment_naming_the_kind_rather_than_the_declared_context_is_dropped() {
        let text =
            "[[bindings]]\ncontext = \"marketdata\"\n[bindings.keys]\n\"i\" = \"cvi::insert\"\n";
        let (kept, diags) = check_fragment(fragment_doc("cvi", text).unwrap(), &["cvi"]);
        assert!(
            kept.table["bindings"].as_array().unwrap().is_empty(),
            "{kept:?}"
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(
            diags[0].message.contains("marketdata"),
            "the diagnostic must name the predicate it refused: {}",
            diags[0].message
        );
        // And the other way round: the same fragment is kept by the
        // factory that really does declare that context.
        let (kept, diags) = check_fragment(fragment_doc("cvi", text).unwrap(), &["marketdata"]);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(kept.table["bindings"].as_array().unwrap().len(), 1);
    }

    /// Negation, disjunction, and parentheses fail the fragment text check,
    /// including otherwise harmless parentheses around the owning context.
    #[test]
    fn a_fragment_predicate_that_is_not_a_plain_conjunction_is_refused() {
        for (predicate, token) in [
            ("!rec", "!"),
            ("rec || workspace", "||"),
            ("(!rec)", "("),
            ("(rec) && mode == normal", "("),
            ("rec && mode != visual", "!"),
        ] {
            let doc = fragment_doc(
                "rec",
                &format!(
                    "[[bindings]]\ncontext = \"{predicate}\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n"
                ),
            )
            .unwrap();
            let (kept, diags) = check_fragment(doc, &["rec"]);
            assert!(
                kept.table["bindings"].as_array().unwrap().is_empty(),
                "{predicate} must bind nothing: {kept:?}"
            );
            assert_eq!(diags.len(), 1, "{predicate}");
            assert_eq!(diags[0].severity, Severity::Error, "{predicate}");
            assert!(
                diags[0].message.contains(predicate)
                    && diags[0].message.contains(&format!("'{token}'")),
                "the diagnostic must name the predicate and the offending token: {}",
                diags[0].message
            );
        }
        // And the conjunction spellings the rule exists to allow still
        // pass, so the refusal is not simply "nothing gets through".
        for predicate in ["rec", "rec && mode == normal"] {
            let doc = fragment_doc(
                "rec",
                &format!(
                    "[[bindings]]\ncontext = \"{predicate}\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n"
                ),
            )
            .unwrap();
            let (kept, diags) = check_fragment(doc, &["rec"]);
            assert!(diags.is_empty(), "{predicate}: {diags:?}");
            assert_eq!(kept.table["bindings"].as_array().unwrap().len(), 1);
        }
    }

    /// A multi-context module keeps a binding in each of its own contexts
    /// and only those — the blotter's `normal`/`visual` shape generalised,
    /// and the reason `contexts` is a list at all.
    #[test]
    fn every_declared_context_is_accepted_and_nothing_else_is() {
        let doc = fragment_doc(
            "rec",
            "[[bindings]]\ncontext = \"rec && mode == normal\"\n[bindings.keys]\n\"j\" = \"rec::down\"\n\n[[bindings]]\ncontext = \"rec-aux\"\n[bindings.keys]\n\"k\" = \"rec::up\"\n\n[[bindings]]\ncontext = \"tile\"\n[bindings.keys]\n\"l\" = \"rec::right\"\n",
        )
        .unwrap();
        let (kept, diags) = check_fragment(doc, &["rec", "rec-aux"]);
        assert_eq!(kept.table["bindings"].as_array().unwrap().len(), 2);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("tile"), "{}", diags[0].message);
    }

    #[test]
    fn a_fragment_that_does_not_parse_is_one_error_diagnostic() {
        let err = fragment_doc("rec", "[[bindings]]\ncontext = \n").unwrap_err();
        assert_eq!(err.severity, Severity::Error);
        assert_eq!(err.layer, Some(Layer::Builtin));
        assert_eq!(err.file, Some(fragment_file("rec")));
        assert!(err.message.contains("rec"), "{}", err.message);
    }

    #[test]
    fn splice_puts_fragments_after_the_builtin_docs_and_before_desk_and_user() {
        let builtin = LayerDoc::builtin("keymap", "").unwrap();
        let desk = LayerDoc {
            layer: Layer::Desk,
            name: "keymap".into(),
            file: "/desk/keymap.toml".into(),
            table: toml::Table::new(),
        };
        let user = LayerDoc {
            layer: Layer::User,
            name: "keymap".into(),
            file: "/user/keymap.toml".into(),
            table: toml::Table::new(),
        };
        let frag = fragment_doc("rec", "").unwrap();
        let out = splice(
            &[builtin.clone(), desk.clone(), user.clone()],
            std::slice::from_ref(&frag),
        );
        let files: Vec<String> = out
            .iter()
            .map(|d| d.file.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files,
            vec![
                builtin.file.to_string_lossy().to_string(),
                "<module:rec>".to_string(),
                "/desk/keymap.toml".to_string(),
                "/user/keymap.toml".to_string()
            ]
        );
    }

    /// All builtin documents precede fragments, even when there is more than one.
    #[test]
    fn splice_keeps_every_builtin_doc_ahead_of_every_fragment() {
        let a = LayerDoc {
            layer: Layer::Builtin,
            name: "keymap".into(),
            file: "<builtin:a>".into(),
            table: toml::Table::new(),
        };
        let user = LayerDoc {
            layer: Layer::User,
            name: "keymap".into(),
            file: "/user/keymap.toml".into(),
            table: toml::Table::new(),
        };
        let b = LayerDoc {
            layer: Layer::Builtin,
            name: "keymap".into(),
            file: "<builtin:b>".into(),
            table: toml::Table::new(),
        };
        let out = splice(&[a, user, b], &[fragment_doc("rec", "").unwrap()]);
        let files: Vec<String> = out
            .iter()
            .map(|d| d.file.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files,
            vec![
                "<builtin:a>",
                "<builtin:b>",
                "<module:rec>",
                "/user/keymap.toml"
            ]
        );
    }

    /// Without fragments, an already ordered document stack is unchanged.
    #[test]
    fn splice_with_no_fragments_changes_nothing() {
        let docs = vec![
            LayerDoc::builtin("keymap", "").unwrap(),
            LayerDoc {
                layer: Layer::User,
                name: "keymap".into(),
                file: "/user/keymap.toml".into(),
                table: toml::Table::new(),
            },
        ];
        let out = splice(&docs, &[]);
        assert_eq!(
            out.iter()
                .map(|d| d.file.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            docs.iter()
                .map(|d| d.file.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_first_identifier_is_the_leading_context_name() {
        assert_eq!(
            first_identifier("blotter && mode == normal"),
            Some("blotter")
        );
        assert_eq!(first_identifier("  (rec-aux)"), Some("rec-aux"));
        assert_eq!(first_identifier("mode == normal"), Some("mode"));
        assert_eq!(first_identifier(""), None);
        assert_eq!(first_identifier("&&"), None);
    }
}
