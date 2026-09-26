//! The `:` vocabulary (spec §9.9), pure. Every word is tile-local
//! (command-line locality); completions are the bare word per position.

use crate::core::{Colour, Range, Rgb8};
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
    Remove(u8),
    Rule(u8, BucketRule),
    Colour(u8, String),
    AxisMode(AxisMode),
    Freq(Frequency),
    Range(Range),
    /// Fractions; empty = off.
    Pct(Vec<f64>),
    Density(Option<u32>),
    YAxis(u8, Axis),
    Split(f32),
    Clear,
}

pub const VERBS: &[&str] = &[
    "add", "expr", "remove", "rule", "colour", "axis", "freq", "range", "pct", "density", "yaxis",
    "split", "clear",
];

/// `:colour`'s colour word: `1`..`5` is a palette index, `#rrggbb` an
/// absolute colour, anything else a `[colours]` name `has_name` knows.
/// `#` is checked before the name because no `[colours]` name may start
/// with one (`geode_core::colour::RESERVED_PREFIX`).
pub fn colour_arg(word: &str, has_name: impl Fn(&str) -> bool) -> Result<Colour, String> {
    let len = geode_chart::core::palette::Palette::LEN;
    if let Ok(i) = word.parse::<usize>()
        && (1..=len).contains(&i)
    {
        return Ok(Colour::Palette(i - 1));
    }
    if word.starts_with(geode_core::colour::RESERVED_PREFIX) {
        return Rgb8::parse_hex(word)
            .map(Colour::Custom)
            .ok_or_else(|| format!("'{word}' is not a colour — #rrggbb, six hex digits"));
    }
    if has_name(word) {
        Ok(Colour::Named(word.into()))
    } else {
        Err(format!(
            "no colour named '{word}' — 1..{len}, a [colours] entry or #rrggbb"
        ))
    }
}

fn slot(word: &str, form: &str) -> Result<u8, String> {
    word.strip_prefix('s')
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| form.to_string())
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
            let [s] = words.as_slice() else {
                return Err("remove s<n>".into());
            };
            Ok(Command::Remove(slot(s, "remove s<n>")?))
        }
        "rule" => {
            const FORM: &str = "rule s<n> last|first|mean|min|max";
            let [s, r] = words.as_slice() else {
                return Err(FORM.into());
            };
            Ok(Command::Rule(
                slot(s, FORM)?,
                BucketRule::parse(r).ok_or(FORM)?,
            ))
        }
        "colour" => {
            const FORM: &str = "colour s<n> <1..5|name|#rrggbb>";
            let [s, c] = words.as_slice() else {
                return Err(FORM.into());
            };
            Ok(Command::Colour(slot(s, FORM)?, c.to_string()))
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
            const FORM: &str = "yaxis s<n> left|right|bottomleft|bottomright";
            let [s, a] = words.as_slice() else {
                return Err(FORM.into());
            };
            Ok(Command::YAxis(slot(s, FORM)?, Axis::parse(a).ok_or(FORM)?))
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
/// `sources` is accepted for symmetry with the `:add` form but the
/// identity position offers nothing (the picker owns identities); keep
/// the parameter — the tile's own `completions` passes it and a later
/// catalogue-backed completion is one arm away.
pub fn completions(
    line: &str,
    cursor: usize,
    slots: &[u8],
    sources: &[String],
    colours: &[String],
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
    let slot_words = || slots.iter().map(|n| format!("s{n}")).collect::<Vec<_>>();
    match (position, verb) {
        (0, _) => s(VERBS),
        (1, "remove" | "rule" | "colour" | "yaxis") => slot_words(),
        (2, "rule") => BucketRule::ALL
            .iter()
            .map(|r| r.as_str().to_string())
            .collect(),
        (2, "colour") => colours
            .iter()
            .cloned()
            .chain((1..=geode_chart::core::palette::Palette::LEN).map(|i| i.to_string()))
            .collect(),
        (2, "yaxis") => Axis::ALL.iter().map(|a| a.as_str().to_string()).collect(),
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
            parse("expr s1 / s2 * 100").unwrap(),
            Command::Expr("s1 / s2 * 100".into())
        );
        assert_eq!(parse("remove s3").unwrap(), Command::Remove(3));
        assert_eq!(
            parse("rule s2 mean").unwrap(),
            Command::Rule(2, BucketRule::Mean)
        );
        assert_eq!(
            parse("colour s1 spx").unwrap(),
            Command::Colour(1, "spx".into())
        );
        assert_eq!(
            parse("colour s1 #FF8800").unwrap(),
            Command::Colour(1, "#FF8800".into())
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
            parse("yaxis s2 bottomright").unwrap(),
            Command::YAxis(2, Axis::BottomRight)
        );
        assert_eq!(parse("split 0.6").unwrap(), Command::Split(0.6));
        assert_eq!(parse("clear").unwrap(), Command::Clear);
        for v in VERBS {
            let line = match *v {
                "add" => "add X",
                "expr" => "expr s1",
                "remove" => "remove s1",
                "rule" => "rule s1 last",
                "colour" => "colour s1 x",
                "axis" => "axis session",
                "freq" => "freq 1d",
                "range" => "range 1y",
                "pct" => "pct off",
                "density" => "density off",
                "yaxis" => "yaxis s1 left",
                "split" => "split 0.7",
                "clear" => "clear",
                _ => unreachable!(),
            };
            assert!(parse(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn a_colour_word_is_an_index_a_hex_or_a_known_name() {
        let known = |n: &str| n == "spx";
        assert_eq!(colour_arg("2", known), Ok(Colour::Palette(1)));
        assert_eq!(
            colour_arg("#FF8800", known),
            Ok(Colour::Custom(Rgb8([0xff, 0x88, 0x00])))
        );
        assert_eq!(colour_arg("spx", known), Ok(Colour::Named("spx".into())));
        for bad in ["#ff88", "#ff88001", "#gg8800", "#"] {
            assert_eq!(
                colour_arg(bad, known),
                Err(format!("'{bad}' is not a colour — #rrggbb, six hex digits")),
                "{bad}"
            );
        }
        // Even a name the colours doc somehow held is never read for a
        // `#` word.
        assert!(colour_arg("#ff88", |_| true).is_err());
        assert_eq!(
            colour_arg("nope", known),
            Err("no colour named 'nope' — 1..5, a [colours] entry or #rrggbb".into())
        );
        assert_eq!(
            parse("colour s1").unwrap_err(),
            "colour s<n> <1..5|name|#rrggbb>"
        );
    }

    #[test]
    fn refusals_name_the_form() {
        assert_eq!(
            parse("").unwrap_err(),
            "commands: add expr remove rule colour axis freq range pct density yaxis split clear"
        );
        assert!(
            parse("bogus")
                .unwrap_err()
                .starts_with("unknown command 'bogus'")
        );
        assert_eq!(parse("add").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("add a@b@c").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("remove 3").unwrap_err(), "remove s<n>");
        assert_eq!(
            parse("rule s2 median").unwrap_err(),
            "rule s<n> last|first|mean|min|max"
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
            parse("yaxis s1 up").unwrap_err(),
            "yaxis s<n> left|right|bottomleft|bottomright"
        );
        assert_eq!(parse("split x").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("split 0.9").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("expr").unwrap_err(), "expr <text>");
        assert_eq!(parse("clear now").unwrap_err(), "clear takes nothing");
    }

    #[test]
    fn completions_are_the_bare_word_per_position() {
        let slots = [1u8, 3];
        let sources = ["demo_kdb".to_string(), "demo_rest".to_string()];
        let colours = ["spx".to_string()];
        assert_eq!(
            completions("", 0, &slots, &sources, &colours),
            VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            completions("ru", 2, &slots, &sources, &colours),
            VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "the shell ranks; the tile answers the whole vocabulary"
        );
        assert_eq!(
            completions("rule ", 5, &slots, &sources, &colours),
            vec!["s1", "s3"]
        );
        assert_eq!(
            completions("rule s1 ", 8, &slots, &sources, &colours),
            vec!["last", "first", "mean", "min", "max"]
        );
        assert_eq!(
            completions("colour s1 ", 10, &slots, &sources, &colours),
            vec!["spx", "1", "2", "3", "4", "5"]
        );
        assert_eq!(
            completions("add ", 4, &slots, &sources, &colours),
            Vec::<String>::new(),
            "identities are the picker's; nothing to offer here"
        );
        assert_eq!(
            completions("axis ", 5, &slots, &sources, &colours),
            vec!["session", "time"]
        );
        assert_eq!(
            completions("freq ", 5, &slots, &sources, &colours),
            vec!["1m", "5m", "15m", "1h", "1d", "1w"]
        );
        assert_eq!(
            completions("range ", 6, &slots, &sources, &colours),
            vec!["1w", "1m", "3m", "6m", "1y", "2y", "5y"]
        );
        assert_eq!(
            completions("pct ", 4, &slots, &sources, &colours),
            vec!["off"]
        );
        assert_eq!(
            completions("density ", 8, &slots, &sources, &colours),
            vec!["off"]
        );
        assert_eq!(
            completions("yaxis s3 ", 9, &slots, &sources, &colours),
            vec!["left", "right", "bottomleft", "bottomright"]
        );
        assert_eq!(
            completions("clear ", 6, &slots, &sources, &colours),
            Vec::<String>::new()
        );
    }

    #[test]
    fn completions_clamp_a_cursor_inside_a_multibyte_char() {
        let slots = [1u8, 3];
        let sources = ["demo_kdb".to_string()];
        let colours = ["spx".to_string()];
        let line = "colour s1 café";
        // `é` is two bytes; this cursor lands one byte past its start,
        // inside the character, not on a char boundary.
        let cursor = line.find('é').unwrap() + 1;
        assert_eq!(
            completions(line, cursor, &slots, &sources, &colours),
            vec!["spx", "1", "2", "3", "4", "5"]
        );
    }
}
