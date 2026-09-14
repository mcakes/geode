//! Keymap fragments: a module ships its own default bindings (market-data
//! documents spec §8.4).
//!
//! The shell cannot depend on a module crate (CLAUDE.md layering), so for
//! three phases the blotter's and the diagnostics tile's `[[bindings]]`
//! lived in [`crate::defaults::BUILTIN_KEYMAP`] beside two mirrored
//! `(id, title)` action tables, kept honest by a cross-crate test on each
//! side. That was a copy of a module's vocabulary inside the shell, and a
//! copy drifts: nothing but those mirror tests connected the two, and a
//! new module could not ship a single default binding without an edit to
//! the shell's own defaults.
//!
//! A fragment inverts it. [`ModuleFactory::default_keymap`] hands the
//! *app* a `[[bindings]]`-only keymap document and the app splices it
//! into the layer stack it already passes to
//! [`build_keymap`](crate::keymap::build_keymap): above the built-in
//! keymap — the shell binds nothing into a module's context any more, so
//! there is nothing there to conflict with — and below every layer a
//! trader edits, so a desk or user keymap overrides a module's default
//! exactly the way it overrides a shell one, through the same layer
//! precedence and with the same `Layer` reported to the keybindings
//! dialog.
//!
//! A fragment doc reports [`Layer::Builtin`], deliberately: to a trader
//! it IS part of what the binary shipped (they cannot edit it, and it
//! sits at the bottom of the precedence order beside the shell's own
//! defaults), so the keybindings dialog's `r` restores it and `d` records
//! a user-layer `"none"` shadow over it — no third layer kind, and no
//! dialog branch that has to know a module exists.
//!
//! **A fragment can never shadow a shell binding or another module's, and
//! [`check_fragment`] is the rule that delivers it: a fragment predicate
//! must be a plain CONJUNCTION whose first identifier is one of its
//! factory's own [`contexts`](crate::module::ModuleFactory::contexts).**
//! The two halves are one promise. With `&&` as the only connective, a
//! first identifier in `contexts` makes the whole predicate *require*
//! that context, so the binding cannot fire outside the module's own
//! tile — which is why `!`, `||` and `(` are refused anywhere in the text
//! (`blotter || workspace` fires everywhere; `(!blotter)` fires
//! everywhere but the blotter). Without the check a module could bind in
//! `workspace` — or inside another module's context — from a layer that
//! appears in no file the trader can open, which is exactly the invisible
//! shadowing the layer order above exists to prevent.
//!
//! [`ModuleFactory::default_keymap`]: crate::module::ModuleFactory::default_keymap

use geode_core::config::{Diagnostic, Layer, LayerDoc};
use std::path::PathBuf;

/// The `file` a fragment doc carries: not a path, because a fragment is
/// compiled into a module crate and has no file — but the same `<…>`
/// shape [`LayerDoc::builtin`] already uses for the compiled-in layer, so
/// every diagnostic printer, the diagnostics tile's config section and
/// the keybindings dialog name the module a binding came from without one
/// of them learning a new case.
pub fn fragment_file(kind: &str) -> PathBuf {
    PathBuf::from(format!("<module:{kind}>"))
}

/// Parse one module's fragment text into a `Builtin`-layer `keymap` doc.
///
/// `Err` is a malformed fragment — a compiled-in authoring mistake, so it
/// can only ever be seen by whoever wrote the module — reported as one
/// error diagnostic rather than a panic, for the same reason
/// `Config::load` never panics on a bad file (spec §10.1): an app that
/// refuses to start is strictly worse than one that starts with a
/// module's keys missing and says so in the diagnostics tile.
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

/// Drop every `[[bindings]]` entry that is not a plain conjunction naming
/// one of `contexts` as its first identifier, with an error diagnostic
/// each.
///
/// The kept doc is otherwise untouched: `build_keymap` still validates
/// the keys, the actions and the predicate itself, and still reports its
/// own diagnostics against the same `file`. This function answers exactly
/// one question — *may this module bind here at all* — because that is
/// the one question `build_keymap` cannot answer: it has never heard of a
/// module.
///
/// **A fragment predicate may only be a conjunction** — `ctx`, or
/// `ctx && key == value`, and nothing else. Any `!`, `||` or `(` anywhere
/// in the text is refused outright, naming the token. That restriction IS
/// the no-shadowing promise: with `&&` alone, a first identifier that is
/// one of the module's own contexts makes the WHOLE predicate require
/// that context, so the binding can only ever fire inside this module's
/// own tile. Every other connective breaks that implication, and the
/// first-identifier scan cannot see it:
///
/// * `blotter || workspace` — first identifier `blotter`, and
///   `Predicate::Or` fires whenever `workspace` is on the stack, which it
///   always is.
/// * `(!blotter)` — a leading `(` hides the negation from any
///   "starts with `!`" guard, and `Predicate::Not` then matches every
///   context EXCEPT the module's own.
/// * `(blotter) && mode == normal` — harmless in itself, refused by the
///   same rule rather than by a parenthesis-aware special case: a
///   textual check that tries to decide which parentheses are safe is
///   exactly the check that got the two above wrong. Spell it without.
///
/// Refused on the text rather than on a parsed
/// [`Predicate`](crate::keymap::Predicate) deliberately: the alternative
/// is evaluating the compiled tree against probe stacks to see where it
/// fires, which is a search over contexts this function does not know
/// (any module's, any future shell context) and would answer "seems safe"
/// rather than "is a conjunction".
///
/// An entry with no `context` at all is dropped for the same reason: a
/// context-free binding applies in every context on the stack, the
/// shell's own included.
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

/// The connective that makes `predicate` more than a conjunction, if it
/// has one: `"!"`, `"||"` or `"("`, whichever appears first in the text.
///
/// A closing `)` is not listed: it cannot appear without an opening one in
/// anything `parse_predicate` accepts, and `build_keymap`'s own
/// `invalid context` diagnostic covers the unbalanced case. `&&` and
/// `==`/`!=` are the whole of what remains — note that `!=` contains a
/// `!`, so it is refused too: `mode != visual` inside a fragment is a
/// negation of the same kind, and `mode == normal` says what a module
/// actually means.
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

/// The first identifier of a context predicate: `blotter` in
/// `blotter && mode == normal`.
///
/// Deliberately a scan rather than a walk of the parsed
/// [`Predicate`](crate::keymap::Predicate): a compiled predicate is a
/// boolean tree with no notion of "the context this binding is about" —
/// `blotter && mode == normal` and `mode == normal && blotter` are the
/// same tree — while the fragment rule is about the spelling a module
/// author wrote and a reader of the file sees first. Identifier
/// characters match the predicate tokenizer's own rule (alphanumeric,
/// `_`, `-`, so kebab-case context names work), so a name this returns
/// is the same name `parse_predicate` reads there.
fn first_identifier(predicate: &str) -> Option<&str> {
    let start = predicate.find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
    let rest = &predicate[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Builtin docs, then `fragments`, then every doc a trader edits.
///
/// `layered` is what `Config::layered_docs("keymap")` hands over, already
/// in `Builtin → Desk → User` order; this is the one place a module's
/// defaults enter that order, and it is a pure function over the two
/// lists so the ordering can be tested without a config directory.
/// Partitioning on [`Layer::Builtin`] rather than splicing at a fixed
/// index is future-proofing, not a live case: exactly one compiled-in
/// keymap doc exists today (`defaults::BUILTIN_KEYMAP` — `--demo`'s
/// generated desk layer ships `app`, `datasets`, `dimensions`,
/// `groupings`, `sources` and `views`, but no `keymap`). Should a second
/// one ever ship, a fixed index would bury it under the fragments, where
/// the partition keeps every builtin doc ahead of them by construction.
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

    /// A module's kind and its key context need not be the same string —
    /// the market-data panel's factory is kind `cvi` and context
    /// `marketdata` (Task 6) — so the check is against `contexts()`, and
    /// a fragment naming the KIND where the factory declares a different
    /// context is dropped like any other foreign context. This is the
    /// case a check written against `kind()` would wave through on every
    /// module whose two names happen to agree and get wrong on the one
    /// where they do not.
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

    /// Only a conjunction confines a binding to the context its first
    /// identifier names, so every other connective is refused — each of
    /// these spellings passes the first-identifier membership test and
    /// binds OUTSIDE the module's own tile (or, for the last, is merely
    /// parenthesised, and refused by the same rule rather than by a
    /// parenthesis-aware special case that would have to get the first
    /// two right too).
    ///
    /// `blotter || workspace`: `Predicate::Or` fires whenever `workspace`
    /// is on the stack, which it always is. `(!blotter)`: the leading `(`
    /// hides the negation from a "starts with `!`" guard, and
    /// `Predicate::Not` then matches every context except the module's.
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

    /// Every compiled-in doc stays ahead of every fragment, whatever
    /// order they arrive in. Future-proofing rather than a live case —
    /// exactly one builtin keymap doc ships today — but a splice at a
    /// fixed index would bury a second compiled-in keymap doc, should one
    /// ever ship, under the fragments; the partition cannot.
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

    /// No fragments is the identity: every shell-only build, every test
    /// fixture and every module-less binary goes through this call.
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
