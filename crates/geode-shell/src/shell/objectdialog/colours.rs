//! The `Domain::Colours` adapter (Part 2c spec §6.1) — the shared colour
//! vocabulary a column's `colour` field and a chart series can name
//! (`geode_core::colour::NamedColours`), edited as one object per named
//! colour: a `hue` on the canonical wheel (with its `tone`), or a
//! `token` naming one of the active theme's own semantic colours.
//!
//! ## Only three keys, never four
//!
//! `hue`+`tone` and `token` are mutually exclusive in the reader
//! (`NamedColours::from_doc`'s own `refuse_both` diagnostic) — a colour
//! is one or the other, never both. [`to_table`] keeps that true on
//! every write by removing all three keys first and then writing back
//! only the ones the current choice is actually made of: `token` alone
//! when it is not `"none"`, otherwise `hue` and — only when the tone is
//! `light` — `tone`. There is no third state to represent: a hue whose
//! tone is `normal` never writes `tone` at all, since `normal` is the
//! reader's own default and an explicit `tone = "normal"` would be a
//! silent no-op key nobody asked for.
//!
//! ## Reserved names refused before they reach a write
//!
//! `none`/`sign` are reserved by a column's own `colour` field grammar
//! (a column can say "no colour" or "follow the sign" without naming an
//! object here) — `geode_core::colour::RESERVED_NAMES`. The reader
//! already drops a colour by either name with a diagnostic
//! (`NamedColours::from_doc`); [`super::Domain::reserved_names`] makes
//! [`super::Domain::name_taken`] refuse the name a keystroke earlier, so
//! `n` never reaches a write for one at all — see `render.rs`'s
//! `create_from_name`, whose reserved branch reads this list.
//!
//! ## The live swatch
//!
//! [`definition_of`] reads the draft's three fields back into a
//! [`geode_core::colour::Definition`] — the same shape [`fields`] built
//! them from — so `render.rs`'s edit-header swatch can resolve it
//! against the active theme without re-parsing `draft.source`. The
//! browse row's own swatch does not go through this at all: it resolves
//! each row's *saved* colour straight off the merged `colours.toml`
//! (`NamedColours::from_doc`), since a browse row has no draft.

use geode_core::colour::{Definition, NamedColours, Token, Tone};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};

use super::{Destination, Draft, Field, FieldKind};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "colours";

/// The browse row's muted second line: `Definition::summary`, read
/// through the very reader that decides what a colour resolves to
/// (`NamedColours::from_doc`) rather than a bespoke re-parse of the raw
/// table — the one-entry-doc trick `sources::summary`'s sibling
/// adapters use for a domain whose reader already does the validating,
/// wrapped instead of duplicated. `"invalid"` for anything the reader
/// drops (both keys, neither key, an out-of-range hue, an unknown
/// token) — the row still needs something to paint, and this is the one
/// word every failure mode collapses to.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "invalid".to_string();
    };
    let mut doc = geode_core::config::MergedDoc::default();
    doc.value
        .insert("x".to_string(), toml::Value::Table(table.clone()));
    let (colours, _) = NamedColours::from_doc(&doc);
    colours
        .get("x")
        .map(Definition::summary)
        .unwrap_or_else(|| "invalid".to_string())
}

/// The three fields of one colour, or of no colour at all when `object`
/// names nothing (`n`'s empty draft, and the schema `n` opens into
/// before a name is even typed): `hue` defaults to `0`, `tone` to
/// `normal`, `token` to `none` — exactly
/// [`geode_core::colour::Definition::Hue`] `{ degrees: 0.0, tone: Normal
/// }`, the default [`definition_of`] reads back from a fresh draft
/// (pinned by `reserved_names_are_taken`'s own `fresh` assertion).
///
/// `hue` reads either an integer or a float off the raw table — the
/// reader accepts both (`h.as_float().or_else(|| h.as_integer()...)`)
/// — so this does too, rather than silently defaulting to `0` for a
/// hand-edited `hue = 210` some other tool wrote as an integer.
/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)).
pub fn help(key: &str) -> &'static str {
    match key {
        "hue" => {
            "Position on a 0–360 wheel — red 0, yellow 60, green 120, cyan 180, blue 240, magenta 300 — rendered in each theme's own palette"
        }
        "tone" => "normal, or light for the theme's tint of the same hue",
        "token" => {
            "A theme colour by role instead of a hue — none keeps the hue; a token overrides it"
        }
        _ => "",
    }
}

pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());

    let hue = table
        .and_then(|t| t.get("hue"))
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(0.0);
    let tone_light = matches!(
        table.and_then(|t| t.get("tone")).and_then(|v| v.as_str()),
        Some("light")
    );
    let token = table
        .and_then(|t| t.get("token"))
        .and_then(|v| v.as_str())
        .and_then(Token::parse);

    // `none` first, then every named token in `Token::ALL`'s own order —
    // 16 options total, matching `fields_seed_from_the_definition_and_
    // to_table_writes_only_the_keys_in_force`'s `options.len() == 16`.
    let mut token_options: Vec<String> = vec!["none".to_string()];
    token_options.extend(Token::ALL.iter().map(|t| t.name().to_string()));
    let token_selected = token
        .map(|t| {
            token_options
                .iter()
                .position(|o| o == t.name())
                .unwrap_or(0)
        })
        .unwrap_or(0);

    let field = |key: &str, label: &str, kind: FieldKind| Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        dest: Destination::Doc,
        layer: None,
    };

    vec![
        field(
            "hue",
            "Hue",
            FieldKind::Number {
                value: hue.round() as i64,
                min: 0,
                max: 359,
                step: 15,
                wrap: true,
            },
        ),
        field(
            "tone",
            "Tone",
            FieldKind::Choice {
                options: vec!["normal".to_string(), "light".to_string()],
                selected: usize::from(tone_light),
            },
        ),
        field(
            "token",
            "Token",
            FieldKind::Choice {
                options: token_options,
                selected: token_selected,
            },
        ),
    ]
}

/// The draft's three fields read back as a [`Definition`] — the same
/// shape a saved colour resolves to, for `render.rs`'s edit-header
/// swatch. `None` only if the draft somehow lacks its `hue` row, which
/// [`fields`] never produces.
pub fn definition_of(draft: &Draft) -> Option<Definition> {
    let value = draft.fields.iter().find_map(|f| match (&f.key, &f.kind) {
        (key, FieldKind::Number { value, .. }) if key == "hue" => Some(*value),
        _ => None,
    })?;
    let token = draft.choice("token")?;
    if token != "none" {
        return Token::parse(token).map(Definition::Token);
    }
    let tone = if draft.choice("tone") == Some("light") {
        Tone::Light
    } else {
        Tone::Normal
    };
    Some(Definition::Hue {
        degrees: value as f32,
        tone,
    })
}

/// The draft as `colours.toml` holds it: `draft.source` with every key
/// the field vocabulary owns (`hue`, `tone`, `token`) removed and then
/// rewritten from the current choice alone — this module's own doc
/// comment has the "only three keys, never four" reasoning. Anything
/// the vocabulary does not model survives untouched, the same
/// "preserve what we don't own" rule every other adapter's `to_table`
/// follows — and so does a `hue` the vocabulary DOES model but this
/// save did not step, which is how a hand-edited `hue = 210.5` survives
/// a step of `tone` (the final review's M-2: the field is seeded
/// rounded, so writing it unconditionally rewrote the file).
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut table = super::toml_table_to_edit(&draft.source);
    // Kept, not discarded (the final review's M-2): `fields` seeds the
    // `Number` with `hue.round()`, so a hand-edited `hue = 210.5` reads
    // back as `211` and writing the field on every save would rewrite
    // the file's own value on a step of `tone` or `token` the trader
    // made instead. The saved item goes back verbatim — formatting and
    // all — whenever the field still rounds to it, so only a step of the
    // hue itself replaces it.
    let saved_hue = table.remove("hue");
    table.remove("tone");
    table.remove("token");
    match draft.choice("token") {
        Some(token) if token != "none" => {
            table["token"] = toml_edit::value(token);
        }
        _ => {
            if let Some(value) = draft.fields.iter().find_map(|f| match (&f.key, &f.kind) {
                (key, FieldKind::Number { value, .. }) if key == "hue" => Some(*value),
                _ => None,
            }) {
                let unstepped = saved_hue
                    .as_ref()
                    .and_then(|item| item.as_value())
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                    .is_some_and(|saved| saved.round() as i64 == value);
                match (unstepped, saved_hue) {
                    (true, Some(saved)) => {
                        table.insert("hue", saved);
                    }
                    _ => table["hue"] = toml_edit::value(value),
                }
            }
            if draft.choice("tone") == Some("light") {
                table["tone"] = toml_edit::value("light");
            }
        }
    }
    toml_edit::Item::Table(table)
}

/// Everything wrong with the draft as it stands (spec §7.2): the
/// rendered table, parsed back and read by the very reader that decides
/// what every consumer of a named colour sees (`NamedColours::from_doc`)
/// — on the object being edited alone, wrapped in a document of its
/// own, for the reason `sources::validate` and `scopes::validate` both
/// give for doing the same: validating the whole merged doc would
/// report every other colour's problems against this one.
pub fn validate(draft: &Draft, _config: &Config) -> Vec<Diagnostic> {
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table: rendered_doc_table(draft),
        }],
    );
    let (_named, diags) = NamedColours::from_doc(&doc);
    diags
}

/// The draft's `colours.toml` entry, rendered and parsed back the way
/// the loader would read it off disk — `sources::rendered_doc_table`'s
/// own shape, mirrored here for the same reason.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use geode_core::config::ConfigSources;

    fn config_with_colours(colours: &str) -> Config {
        let colours = LayerDoc::builtin(DOC, colours).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![colours],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn fields_seed_from_the_definition_and_to_table_writes_only_the_keys_in_force() {
        let config = config_with_colours(
            "[delta]\nhue = 240\n[gamma]\nhue = 210\ntone = \"light\"\n\
             [pnl]\ntoken = \"chart.bullish\"\n",
        );
        let draft = Domain::Colours.draft(&config, "gamma");
        let by = |k: &str| draft.fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(
            by("hue").kind,
            FieldKind::Number {
                value: 210,
                min: 0,
                max: 359,
                step: 15,
                wrap: true
            }
        ));
        assert!(
            matches!(&by("tone").kind, FieldKind::Choice { options, selected } if options[*selected] == "light")
        );
        assert!(
            matches!(&by("token").kind, FieldKind::Choice { options, selected } if options[*selected] == "none" && options.len() == 16)
        );
        let text = super::super::object_text("gamma", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("hue = 210")
                && text.contains("tone = \"light\"")
                && !text.contains("token"),
            "{text}"
        );
        let draft = Domain::Colours.draft(&config, "pnl");
        let text = super::super::object_text("pnl", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("token = \"chart.bullish\"") && !text.contains("hue"),
            "no dead hue beside a token: {text}"
        );
        assert_eq!(
            definition_of(&draft),
            Some(Definition::Token(Token::Bullish))
        );

        // The mutation `pnl` alone cannot pin: its `source` never held a
        // `hue` in the first place, so a `to_table` that forgot to
        // remove one would still render correctly by sheer absence.
        // Switching a hue-based colour's own draft to a token — `gamma`,
        // still carrying `hue = 210` and `tone = "light"` in `source` —
        // is the case that actually exercises the removal: without it,
        // the stale `hue`/`tone` from `source` would leak straight
        // through `toml_table_to_edit`, dead beside the new `token`.
        let mut draft = Domain::Colours.draft(&config, "gamma");
        let token_field = draft.fields.iter_mut().find(|f| f.key == "token").unwrap();
        if let FieldKind::Choice { options, selected } = &mut token_field.kind {
            *selected = options
                .iter()
                .position(|o| o == "chart.bullish")
                .expect("chart.bullish is one of the 16 options");
        }
        let text = super::super::object_text("gamma", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("token = \"chart.bullish\"")
                && !text.contains("hue")
                && !text.contains("tone"),
            "switching to a token must drop the old hue and tone: {text}"
        );
    }

    /// M-2 (Part 2c final review): a hand-edited fractional `hue` is the
    /// trader's own value, and a save that did not step the hue must
    /// leave it alone.
    ///
    /// [`fields`] seeds the `Number` with `hue.round()` — the field
    /// vocabulary has no fractional step — so writing the field's value
    /// on every save rewrote `hue = 210.5` to `hue = 211` the first time
    /// the trader touched `tone` or `token`, without them ever pressing
    /// a key on the hue row. Stepping the hue itself still writes it,
    /// which is the other half asserted here: the guard is "did this
    /// save move the hue", never "is the source fractional".
    #[test]
    fn a_fractional_hue_survives_a_save_that_did_not_step_it() {
        let config = config_with_colours("[gamma]\nhue = 210.5\n");

        // A step of `tone` alone: the hue row was never touched.
        let mut draft = Domain::Colours.draft(&config, "gamma");
        let tone = draft.fields.iter_mut().find(|f| f.key == "tone").unwrap();
        if let FieldKind::Choice { options, selected } = &mut tone.kind {
            *selected = options.iter().position(|o| o == "light").unwrap();
        }
        let text = super::super::object_text("gamma", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("hue = 210.5") && text.contains("tone = \"light\""),
            "a step of tone rewrote the hand-edited hue: {text}"
        );

        // And a step of the hue itself does write the field's value —
        // the rounded one, since that is what the trader stepped from.
        let mut draft = Domain::Colours.draft(&config, "gamma");
        let hue = draft.fields.iter_mut().find(|f| f.key == "hue").unwrap();
        if let FieldKind::Number { value, .. } = &mut hue.kind {
            *value += 15;
        }
        let text = super::super::object_text("gamma", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("hue = 226") && !text.contains("210.5"),
            "stepping the hue must write the stepped value — 211, the \
             rounded seed, plus one step: {text}"
        );
    }

    #[test]
    fn reserved_names_are_taken() {
        let config = config_with_colours("[delta]\nhue = 240\n");
        assert!(
            Domain::Colours.name_taken(&config, "sign")
                && Domain::Colours.name_taken(&config, "none")
        );
        assert!(!Domain::Views.name_taken(&config, "sign"));
        let draft = Domain::Colours.new_draft(&config, "fresh");
        assert_eq!(
            definition_of(&draft),
            Some(Definition::Hue {
                degrees: 0.0,
                tone: Tone::Normal
            })
        );
    }
}
