//! The `:` vocabulary (spec §9.9), pure. Every word is tile-local
//! (command-line locality); completions are the bare word per position.

use crate::core::{Color, Range, Rgb8};
use geode_chart::core::layout::{SPLIT_MAX, SPLIT_MIN};
use geode_chart::{Axis, AxisMode};
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Add {
        identity: String,
        source: Option<String>,
    },
    Expr(String),
    /// The series verbs name their target first, or act on the selected
    /// series when the name is left out (`None`); the word count tells
    /// the two apart.
    Remove(Option<String>),
    Rule(Option<String>, BucketRule),
    Color(Option<String>, String),
    AxisMode(AxisMode),
    Freq(Frequency),
    Range(Range),
    /// Fractions; empty = off.
    Pct(Vec<f64>),
    Density(Option<u32>),
    YAxis(Option<String>, Axis),
    Split(f32),
    Clear,
}

pub const VERBS: &[&str] = &[
    "add", "expr", "remove", "rule", "color", "axis", "freq", "range", "pct", "density", "yaxis",
    "split", "clear",
];

/// `:color`'s color word: `1`..`5` is a palette index, `#rrggbb` an
/// absolute color, anything else a `[colors]` name `has_name` knows.
/// `#` is checked before the name because no `[colors]` name may start
/// with one (`geode_core::colour::RESERVED_PREFIX`).
pub fn color_arg(word: &str, has_name: impl Fn(&str) -> bool) -> Result<Color, String> {
    let len = geode_chart::core::palette::Palette::LEN;
    if let Ok(i) = word.parse::<usize>()
        && (1..=len).contains(&i)
    {
        return Ok(Color::Palette(i - 1));
    }
    if word.starts_with(geode_core::colour::RESERVED_PREFIX) {
        return Rgb8::parse_hex(word)
            .map(Color::Custom)
            .ok_or_else(|| format!("'{word}' is not a color — #rrggbb, six hex digits"));
    }
    if has_name(word) {
        Ok(Color::Named(word.into()))
    } else {
        Err(format!(
            "no color named '{word}' — 1..{len}, a [colors] entry or #rrggbb"
        ))
    }
}

/// A series verb's words: `[name] <value>` when the verb takes a value
/// (`value` true), else `[name]`. Answers the name, if given, and the
/// value word.
fn named<'a>(
    words: &[&'a str],
    value: bool,
    form: &str,
) -> Result<(Option<String>, &'a str), String> {
    match (words, value) {
        ([], false) => Ok((None, "")),
        ([name], false) => Ok((Some((*name).to_string()), "")),
        ([v], true) => Ok((None, v)),
        ([name, v], true) => Ok((Some((*name).to_string()), v)),
        _ => Err(form.to_string()),
    }
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line
        .split_once(char::is_whitespace)
        .map(|(v, r)| (v, r.trim()))
        .unwrap_or((line, ""));
    let words: Vec<&str> = rest.split_whitespace().collect();
    match verb {
        "" => Err(format!("commands: {}", VERBS.join(" "))),
        "add" => {
            const FORM: &str = "add <identity>[@source]";
            let [w] = words.as_slice() else {
                return Err(FORM.into());
            };
            let mut parts = w.split('@');
            let identity = parts
                .next()
                .filter(|s| !s.is_empty())
                .ok_or(FORM)?
                .to_string();
            let source = parts.next().map(str::to_string);
            if parts.next().is_some() || source.as_deref() == Some("") {
                return Err(FORM.into());
            }
            Ok(Command::Add { identity, source })
        }
        "expr" => {
            if rest.is_empty() {
                Err("expr <text>".into())
            } else {
                Ok(Command::Expr(rest.to_string()))
            }
        }
        "remove" => {
            let (name, _) = named(&words, false, "remove [series]")?;
            Ok(Command::Remove(name))
        }
        "rule" => {
            const FORM: &str = "rule [series] last|first|mean|min|max";
            let (name, r) = named(&words, true, FORM)?;
            Ok(Command::Rule(name, BucketRule::parse(r).ok_or(FORM)?))
        }
        "color" => {
            const FORM: &str = "color [series] <1..5|name|#rrggbb>";
            let (name, c) = named(&words, true, FORM)?;
            Ok(Command::Color(name, c.to_string()))
        }
        "axis" => {
            let [m] = words.as_slice() else {
                return Err("axis session|time".into());
            };
            Ok(Command::AxisMode(
                AxisMode::parse(m).ok_or("axis session|time")?,
            ))
        }
        "freq" => {
            let [f] = words.as_slice() else {
                return Err("freq 1m|5m|15m|1h|1d|1w".into());
            };
            Ok(Command::Freq(
                Frequency::parse(f).ok_or("freq 1m|5m|15m|1h|1d|1w")?,
            ))
        }
        "range" => Range::parse(&words).map(Command::Range),
        "pct" => {
            const FORM: &str = "pct <n>… in (0, 100), or off";
            if words == ["off"] {
                return Ok(Command::Pct(vec![]));
            }
            if words.is_empty() {
                return Err(FORM.into());
            }
            let mut out = Vec::new();
            for w in &words {
                let n: f64 = w.parse().map_err(|_| FORM)?;
                if !(n > 0.0 && n < 100.0) {
                    return Err(FORM.into());
                }
                out.push(n / 100.0);
            }
            Ok(Command::Pct(out))
        }
        "density" => {
            let form = format!("density {MIN_BINS}..={MAX_BINS}, or off");
            let [w] = words.as_slice() else {
                return Err(form);
            };
            if *w == "off" {
                return Ok(Command::Density(None));
            }
            let n: u32 = w.parse().map_err(|_| form.clone())?;
            if !(MIN_BINS..=MAX_BINS).contains(&n) {
                return Err(form);
            }
            Ok(Command::Density(Some(n)))
        }
        "yaxis" => {
            const FORM: &str = "yaxis [series] left|right|bottomleft|bottomright";
            let (name, a) = named(&words, true, FORM)?;
            Ok(Command::YAxis(name, Axis::parse(a).ok_or(FORM)?))
        }
        "split" => {
            let form = format!("split {SPLIT_MIN}..={SPLIT_MAX}");
            let [w] = words.as_slice() else {
                return Err(form);
            };
            let f: f32 = w.parse().map_err(|_| form.clone())?;
            if !(SPLIT_MIN..=SPLIT_MAX).contains(&f) {
                return Err(form);
            }
            Ok(Command::Split(f))
        }
        "clear" => {
            if words.is_empty() {
                Ok(Command::Clear)
            } else {
                Err("clear takes nothing".into())
            }
        }
        other => Err(format!(
            "unknown command '{other}' — commands: {}",
            VERBS.join(" ")
        )),
    }
}

/// The word under `cursor` decides the position: 0 is the verb, then
/// each verb's own positions. Unfiltered; the shell ranks.
///
/// `names` are the source series' labels (`Model::series_names`). A
/// series verb's first word is a name or, with the name left out, its
/// value, so position 1 offers both; position 2 follows a name and
/// offers the values.
///
/// `sources` is accepted for symmetry with the `:add` form but the
/// identity position offers nothing (the picker owns identities); keep
/// the parameter — the tile's own `completions` passes it and a later
/// catalogue-backed completion is one arm away.
pub fn completions(
    line: &str,
    cursor: usize,
    names: &[String],
    sources: &[String],
    colors: &[String],
) -> Vec<String> {
    let _ = sources;
    let mut cursor = cursor.min(line.len());
    // The caller's cursor should always be on a char boundary, but this
    // pure core must not depend on that — clamp down to the nearest
    // boundary at or before it rather than panicking on the slice below
    // (mirrors `commandline::word_at`'s guard for the same case, and
    // `geode-blotter`'s `commands::completions`).
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let head = &line[..cursor];
    let position = head.split_whitespace().count().saturating_sub(
        if head.ends_with(char::is_whitespace) || head.is_empty() {
            0
        } else {
            1
        },
    );
    let verb = head.split_whitespace().next().unwrap_or("");
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let rules = || BucketRule::ALL.iter().map(|r| r.as_str().to_string());
    let color_words = || {
        colors
            .iter()
            .cloned()
            .chain((1..=geode_chart::core::palette::Palette::LEN).map(|i| i.to_string()))
    };
    let axes = || Axis::ALL.iter().map(|a| a.as_str().to_string());
    let then = |values: Vec<String>| names.iter().cloned().chain(values).collect::<Vec<_>>();
    match (position, verb) {
        (0, _) => s(VERBS),
        (1, "remove") => names.to_vec(),
        (1, "rule") => then(rules().collect()),
        (1, "color") => then(color_words().collect()),
        (1, "yaxis") => then(axes().collect()),
        (2, "rule") => rules().collect(),
        (2, "color") => color_words().collect(),
        (2, "yaxis") => axes().collect(),
        (1, "axis") => s(&["session", "time"]),
        (1, "freq") => Frequency::ALL
            .iter()
            .map(|f| f.as_str().to_string())
            .collect(),
        (1, "range") => crate::core::Preset::ALL
            .iter()
            .map(|p| p.as_str().to_string())
            .collect(),
        (1, "pct" | "density") => s(&["off"]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Preset, Range};
    use geode_chart::{Axis, AxisMode};
    use geode_core::series::{BucketRule, Frequency};

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn every_verb_parses_to_its_command() {
        assert_eq!(
            parse("add SPX.close").unwrap(),
            Command::Add {
                identity: "SPX.close".into(),
                source: None
            }
        );
        assert_eq!(
            parse("add SPX.close@demo_rest").unwrap(),
            Command::Add {
                identity: "SPX.close".into(),
                source: Some("demo_rest".into())
            }
        );
        assert_eq!(
            parse("expr SPX.close / VIX * 100").unwrap(),
            Command::Expr("SPX.close / VIX * 100".into())
        );
        assert_eq!(parse("remove").unwrap(), Command::Remove(None));
        assert_eq!(
            parse("remove SPX.close@demo_rest").unwrap(),
            Command::Remove(some("SPX.close@demo_rest"))
        );
        assert_eq!(
            parse("rule mean").unwrap(),
            Command::Rule(None, BucketRule::Mean)
        );
        assert_eq!(
            parse("rule VIX mean").unwrap(),
            Command::Rule(some("VIX"), BucketRule::Mean)
        );
        assert_eq!(
            parse("color spx").unwrap(),
            Command::Color(None, "spx".into()),
            "one word is the color, for the selected series"
        );
        assert_eq!(
            parse("color VIX #FF8800").unwrap(),
            Command::Color(some("VIX"), "#FF8800".into())
        );
        assert_eq!(
            parse("axis time").unwrap(),
            Command::AxisMode(AxisMode::Continuous)
        );
        assert_eq!(parse("freq 1h").unwrap(), Command::Freq(Frequency::H1));
        assert_eq!(
            parse("range 6m").unwrap(),
            Command::Range(Range::Relative(Preset::M6))
        );
        assert!(matches!(
            parse("range 2026-01-05 2026-02-05").unwrap(),
            Command::Range(Range::Absolute { .. })
        ));
        assert_eq!(
            parse("pct 5 50 95").unwrap(),
            Command::Pct(vec![0.05, 0.5, 0.95])
        );
        assert_eq!(parse("pct off").unwrap(), Command::Pct(vec![]));
        assert_eq!(parse("density 40").unwrap(), Command::Density(Some(40)));
        assert_eq!(parse("density off").unwrap(), Command::Density(None));
        assert_eq!(
            parse("yaxis right").unwrap(),
            Command::YAxis(None, Axis::Right)
        );
        assert_eq!(
            parse("yaxis VIX bottomright").unwrap(),
            Command::YAxis(some("VIX"), Axis::BottomRight)
        );
        assert_eq!(parse("split 0.6").unwrap(), Command::Split(0.6));
        assert_eq!(parse("clear").unwrap(), Command::Clear);
        for v in VERBS {
            let line = match *v {
                "add" => "add X",
                "expr" => "expr X",
                "remove" => "remove",
                "rule" => "rule last",
                "color" => "color x",
                "axis" => "axis session",
                "freq" => "freq 1d",
                "range" => "range 1y",
                "pct" => "pct off",
                "density" => "density off",
                "yaxis" => "yaxis left",
                "split" => "split 0.7",
                "clear" => "clear",
                _ => unreachable!(),
            };
            assert!(parse(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn a_color_word_is_an_index_a_hex_or_a_known_name() {
        let known = |n: &str| n == "spx";
        assert_eq!(color_arg("2", known), Ok(Color::Palette(1)));
        assert_eq!(
            color_arg("#FF8800", known),
            Ok(Color::Custom(Rgb8([0xff, 0x88, 0x00])))
        );
        assert_eq!(color_arg("spx", known), Ok(Color::Named("spx".into())));
        for bad in ["#ff88", "#ff88001", "#gg8800", "#"] {
            assert_eq!(
                color_arg(bad, known),
                Err(format!("'{bad}' is not a color — #rrggbb, six hex digits")),
                "{bad}"
            );
        }
        // Even a name the colors doc somehow held is never read for a
        // `#` word.
        assert!(color_arg("#ff88", |_| true).is_err());
        assert_eq!(
            color_arg("nope", known),
            Err("no color named 'nope' — 1..5, a [colors] entry or #rrggbb".into())
        );
        assert_eq!(
            parse("color").unwrap_err(),
            "color [series] <1..5|name|#rrggbb>"
        );
    }

    #[test]
    fn refusals_name_the_form() {
        assert_eq!(
            parse("").unwrap_err(),
            "commands: add expr remove rule color axis freq range pct density yaxis split clear"
        );
        assert!(
            parse("bogus")
                .unwrap_err()
                .starts_with("unknown command 'bogus'")
        );
        // The old spelling is not kept as an alias.
        assert!(
            parse("colour 2")
                .unwrap_err()
                .starts_with("unknown command 'colour'")
        );
        assert_eq!(parse("add").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("add a@b@c").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("remove a b").unwrap_err(), "remove [series]");
        assert_eq!(
            parse("rule VIX median").unwrap_err(),
            "rule [series] last|first|mean|min|max"
        );
        assert_eq!(
            parse("rule").unwrap_err(),
            "rule [series] last|first|mean|min|max"
        );
        assert_eq!(
            parse("color a b c").unwrap_err(),
            "color [series] <1..5|name|#rrggbb>"
        );
        assert_eq!(parse("axis wall").unwrap_err(), "axis session|time");
        assert_eq!(parse("freq 2h").unwrap_err(), "freq 1m|5m|15m|1h|1d|1w");
        assert!(
            parse("range 4m")
                .unwrap_err()
                .contains("1w 1m 3m 6m 1y 2y 5y")
        );
        assert_eq!(
            parse("pct 0 50").unwrap_err(),
            "pct <n>… in (0, 100), or off"
        );
        assert_eq!(parse("pct").unwrap_err(), "pct <n>… in (0, 100), or off");
        assert_eq!(parse("density 3").unwrap_err(), "density 4..=200, or off");
        assert_eq!(
            parse("yaxis VIX up").unwrap_err(),
            "yaxis [series] left|right|bottomleft|bottomright"
        );
        assert_eq!(parse("split x").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("split 0.9").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("expr").unwrap_err(), "expr <text>");
        assert_eq!(parse("clear now").unwrap_err(), "clear takes nothing");
    }

    #[test]
    fn completions_are_the_bare_word_per_position() {
        let names = ["SPX.close".to_string(), "VIX@demo_rest".to_string()];
        let sources = ["demo_kdb".to_string(), "demo_rest".to_string()];
        let colors = ["spx".to_string()];
        let c = |line: &str| completions(line, line.len(), &names, &sources, &colors);
        let with_names = |values: &[&str]| {
            names
                .iter()
                .cloned()
                .chain(values.iter().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            completions("", 0, &names, &sources, &colors),
            VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            completions("ru", 2, &names, &sources, &colors),
            VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "the shell ranks; the tile answers the whole vocabulary"
        );
        assert_eq!(c("remove "), names.to_vec(), "a name, nothing else");
        assert_eq!(
            c("rule "),
            with_names(&["last", "first", "mean", "min", "max"]),
            "a name, or the rule for the selected series"
        );
        assert_eq!(
            c("rule VIX@demo_rest "),
            vec!["last", "first", "mean", "min", "max"]
        );
        assert_eq!(c("color "), with_names(&["spx", "1", "2", "3", "4", "5"]));
        assert_eq!(c("color SPX.close "), vec!["spx", "1", "2", "3", "4", "5"]);
        assert_eq!(
            c("yaxis "),
            with_names(&["left", "right", "bottomleft", "bottomright"])
        );
        assert_eq!(
            c("yaxis VIX@demo_rest "),
            vec!["left", "right", "bottomleft", "bottomright"]
        );
        assert_eq!(
            c("add "),
            Vec::<String>::new(),
            "identities are the picker's; nothing to offer here"
        );
        assert_eq!(c("axis "), vec!["session", "time"]);
        assert_eq!(c("freq "), vec!["1m", "5m", "15m", "1h", "1d", "1w"]);
        assert_eq!(c("range "), vec!["1w", "1m", "3m", "6m", "1y", "2y", "5y"]);
        assert_eq!(c("pct "), vec!["off"]);
        assert_eq!(c("density "), vec!["off"]);
        assert_eq!(c("clear "), Vec::<String>::new());
    }

    #[test]
    fn completions_clamp_a_cursor_inside_a_multibyte_char() {
        let names = ["SPX.close".to_string()];
        let sources = ["demo_kdb".to_string()];
        let colors = ["spx".to_string()];
        let line = "color SPX.close café";
        // `é` is two bytes; this cursor lands one byte past its start,
        // inside the character, not on a char boundary.
        let cursor = line.find('é').unwrap() + 1;
        assert_eq!(
            completions(line, cursor, &names, &sources, &colors),
            vec!["spx", "1", "2", "3", "4", "5"]
        );
    }
}
