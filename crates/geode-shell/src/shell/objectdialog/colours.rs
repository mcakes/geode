//! Adapter for shared named colors: a hue and tone, or a semantic theme token, with
//! optional sign tinting. All fields write the color's definition.
//!
//! The writer emits only the selected base: a token excludes hue and tone; a hue omits
//! the default normal tone. Unticked `tint_sign` is omitted. Unmodelled definition keys
//! survive. A fractional source hue also survives edits to other fields while its
//! rounded numeric field remains unchanged.
//!
//! `none` and `sign` are reserved by column-format syntax and cannot be created as
//! named colors. Edit swatches resolve the draft's current fields; browse swatches
//! resolve the saved definitions against the active theme.

use geode_core::colour::{Definition, NamedColours, Token, Tone};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};

use super::{Destination, Draft, Field, FieldKind};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = geode_core::config::COLORS_DOC;

/// The configured named colors, in declared order: what a column's `color` choice
/// offers beside the reserved names.
pub fn names(config: &Config) -> Vec<String> {
    config
        .doc(DOC)
        .map(|doc| {
            NamedColours::from_doc(doc)
                .0
                .names()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The browse row's muted second line: `Definition::summary`, read
/// through the very reader that decides what a color resolves to
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

/// The four fields of one color, or of no color at all when `object`
/// names nothing (`n`'s empty draft, and the schema `n` opens into
/// before a name is even typed): `hue` defaults to `0`, `tone` to
/// `normal`, `token` to `none`, `tint_sign` to off — exactly
/// [`geode_core::colour::Definition::hue`]`(0.0, Normal)`, the default
/// [`definition_of`] reads back from a fresh draft
/// (pinned by `reserved_names_are_taken`'s own `fresh` assertion).
///
/// `hue` reads either an integer or a float off the raw table — the
/// reader accepts both (`h.as_float().or_else(|| h.as_integer()...)`)
/// — so this does too, rather than silently defaulting to `0` for a
/// hand-edited `hue = 210` some other tool wrote as an integer.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(table)
}

/// [`fields`] read straight off a color's raw table rather than a named object
/// `config` defines: the value-color list's `New named color…` seeds an unwritten
/// draft with its definition (`NameSeed::Definition`). `None` is the empty color.
pub(super) fn fields_from_table(table: Option<&toml::Table>) -> Vec<Field> {
    let hue = table
        .and_then(|t| t.get("hue"))
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(0.0);
    let tone_light = matches!(
        table.and_then(|t| t.get("tone")).and_then(|v| v.as_str()),
        Some("light")
    );
    let tint_sign = table
        .and_then(|t| t.get("tint_sign"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
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
        // Last, after both bases: it applies to either.
        field("tint_sign", "Tint by sign", FieldKind::Bool(tint_sign)),
    ]
}

/// The draft's four fields read back as a [`Definition`] — the same
/// shape a saved color resolves to, for `render.rs`'s edit-header
/// swatch. `None` only if the draft somehow lacks its `hue` row, which
/// [`fields`] never produces.
pub fn definition_of(draft: &Draft) -> Option<Definition> {
    let value = draft.fields.iter().find_map(|f| match (&f.key, &f.kind) {
        (key, FieldKind::Number { value, .. }) if key == "hue" => Some(*value),
        _ => None,
    })?;
    let tint_sign = tint_sign_of(draft);
    let token = draft.choice("token")?;
    let base = if token != "none" {
        Definition::token(Token::parse(token)?)
    } else {
        let tone = if draft.choice("tone") == Some("light") {
            Tone::Light
        } else {
            Tone::Normal
        };
        Definition::hue(value as f32, tone)
    };
    Some(if tint_sign { base.tinted() } else { base })
}

/// The `tint_sign` row's tick; `false` if the draft somehow lacks it.
fn tint_sign_of(draft: &Draft) -> bool {
    draft
        .fields
        .iter()
        .any(|f| f.key == "tint_sign" && matches!(f.kind, FieldKind::Bool(true)))
}

/// Rewrite the keys owned by the four fields while retaining unmodelled keys. Emit only
/// the selected hue/tone or token base and optional sign tinting. Preserve a fractional
/// source hue while its rounded field value is unchanged.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut table = super::toml_table_to_edit(&draft.source);
    // The numeric field holds a rounded hue. Preserve the original fractional value
    // when an edit changes another field; a changed hue writes its new value.
    let saved_hue = table.remove("hue");
    table.remove("tone");
    table.remove("token");
    table.remove("tint_sign");
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
    // Beside either base: `false` is the reader's default and is never
    // written, the same rule `tone = "normal"` follows.
    if tint_sign_of(draft) {
        table["tint_sign"] = toml_edit::value(true);
    }
    toml_edit::Item::Table(table)
}

/// Everything wrong with the draft as it stands: the rendered table, parsed back and
/// read by the very reader that decides what every consumer of a named color sees
/// (`NamedColours::from_doc`) — on the object being edited alone, wrapped in a document
/// of its own, for the reason `sources::validate` gives for
/// doing the same: validating the whole merged doc would report every other color's
/// problems against this one.
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

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)). `token` says "replaces",
/// not "overrides": `to_table` writes only the keys in force, so a
/// chosen token drops `hue` from the file and a reopen seeds it at 0.
pub fn help(key: &str) -> &'static str {
    match key {
        "hue" => {
            "Hue on a 0–360 wheel — red 0, yellow 60, green 120, cyan 180, blue 240, magenta 300"
        }
        "tone" => "normal, or light for the theme's tint of the same hue",
        "tint_sign" => {
            "Positive numbers shift the hue toward cool, negative toward warm — a hint of sign"
        }
        "token" => {
            "A theme color by role — a token replaces the hue in the file; none uses the hue"
        }
        _ => "",
    }
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

    /// `New named color…`'s seed reaches the edit stage through the source
    /// door: the unwritten definition's hue and tone, not the empty color.
    #[test]
    fn colors_fields_from_a_source_table_read_its_definition() {
        let config = config_with_colours("");
        let table = Definition::hue(210.0, Tone::Light).to_table();
        let fields = Domain::Colors.fields_from_source(&config, &table);
        let by = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(
            by("hue").kind,
            FieldKind::Number { value: 210, .. }
        ));
        assert!(
            matches!(&by("tone").kind, FieldKind::Choice { options, selected } if options[*selected] == "light")
        );
    }

    #[test]
    fn fields_seed_from_the_definition_and_to_table_writes_only_the_keys_in_force() {
        let config = config_with_colours(
            "[delta]\nhue = 240\n[gamma]\nhue = 210\ntone = \"light\"\n\
             [pnl]\ntoken = \"chart.bullish\"\n",
        );
        let draft = Domain::Colors.draft(&config, "gamma");
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
        let draft = Domain::Colors.draft(&config, "pnl");
        let text = super::super::object_text("pnl", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("token = \"chart.bullish\"") && !text.contains("hue"),
            "no dead hue beside a token: {text}"
        );
        assert_eq!(
            definition_of(&draft),
            Some(Definition::token(Token::Bullish))
        );

        // The mutation `pnl` alone cannot pin: its `source` never held a
        // `hue` in the first place, so a `to_table` that forgot to
        // remove one would still render correctly by sheer absence.
        // Switching a hue-based color's own draft to a token — `gamma`,
        // still carrying `hue = 210` and `tone = "light"` in `source` —
        // is the case that actually exercises the removal: without it,
        // the stale `hue`/`tone` from `source` would leak straight
        // through `toml_table_to_edit`, dead beside the new `token`.
        let mut draft = Domain::Colors.draft(&config, "gamma");
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

    /// An edit to tone or token preserves the original fractional hue while the rounded
    /// hue field is unchanged. Stepping that field writes the new hue.
    #[test]
    fn a_fractional_hue_survives_a_save_that_did_not_step_it() {
        let config = config_with_colours("[gamma]\nhue = 210.5\n");

        // A step of `tone` alone: the hue row was never touched.
        let mut draft = Domain::Colors.draft(&config, "gamma");
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
        let mut draft = Domain::Colors.draft(&config, "gamma");
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
            Domain::Colors.name_taken(&config, "sign")
                && Domain::Colors.name_taken(&config, "none")
        );
        assert!(!Domain::Views.name_taken(&config, "sign"));
        // `#rrggbb` is an absolute color where a name is also read, so
        // the colors domain refuses a `#` name up front — and only it.
        assert!(Domain::Colors.is_reserved("#ff8800"));
        assert!(Domain::Colors.name_taken(&config, "#ff8800"));
        assert!(!Domain::Views.name_taken(&config, "#ff8800"));
        assert!(!Domain::Colors.is_reserved("a#b"));
        let draft = Domain::Colors.new_draft(&config, "fresh");
        assert_eq!(
            definition_of(&draft),
            Some(Definition::hue(0.0, Tone::Normal))
        );
    }

    /// The fourth row, `tint_sign`: seeded from the file, read back by
    /// `definition_of` beside either a hue or a token, and written only
    /// when true — an explicit `tint_sign = false` is the reader's own
    /// default and would be a no-op key nobody asked for, the same rule
    /// `tone = "normal"` follows.
    #[test]
    fn tint_sign_is_a_bool_row_written_only_when_true() {
        let config = config_with_colours(
            "[delta]\nhue = 240\ntint_sign = true\n[pnl]\ntoken = \"chart.3\"\n",
        );
        let draft = Domain::Colors.draft(&config, "delta");
        let keys: Vec<&str> = draft.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["hue", "tone", "token", "tint_sign"]);
        assert!(matches!(
            draft
                .fields
                .iter()
                .find(|f| f.key == "tint_sign")
                .unwrap()
                .kind,
            FieldKind::Bool(true)
        ));
        assert_eq!(
            definition_of(&draft),
            Some(Definition::hue(240.0, Tone::Normal).tinted())
        );

        // Untick: the key leaves the file, the hue stays.
        let mut draft = Domain::Colors.draft(&config, "delta");
        let row = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "tint_sign")
            .unwrap();
        row.kind = FieldKind::Bool(false);
        let text = super::super::object_text("delta", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("hue = 240") && !text.contains("tint_sign"),
            "{text}"
        );
        assert_eq!(
            definition_of(&draft),
            Some(Definition::hue(240.0, Tone::Normal))
        );

        // Tick on a token: written beside the token, no hue in sight.
        let mut draft = Domain::Colors.draft(&config, "pnl");
        assert!(matches!(
            draft
                .fields
                .iter()
                .find(|f| f.key == "tint_sign")
                .unwrap()
                .kind,
            FieldKind::Bool(false)
        ));
        let row = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "tint_sign")
            .unwrap();
        row.kind = FieldKind::Bool(true);
        let text = super::super::object_text("pnl", to_table(&draft, Destination::Doc));
        assert!(
            text.contains("token = \"chart.3\"")
                && text.contains("tint_sign = true")
                && !text.contains("hue"),
            "{text}"
        );
        assert_eq!(
            definition_of(&draft),
            Some(Definition::token(Token::Chart(3)).tinted())
        );
        assert!(!help("tint_sign").is_empty(), "the row has a help line");
    }
}
