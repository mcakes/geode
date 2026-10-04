//! Pure parsing and completion vocabulary for the tile's `:` line.
//! Commands come back as data; the tile owns every change they make.
//!
//! Vocabulary: `underlying <ref>`, `x <coordinate>`,
//! `diff <kind> - <kind> | off`: a pair toggles that difference (turning
//! it on turns its reverse off), `off` clears every one; `none` is read
//! as `off`; and `ylim <lo> <hi> | off`: the differences axis's fixed y
//! domain, in the axis's own units or with a `%` suffix per value (`-2%`
//! is `-0.02`), and `off` (or `auto`) autoscales it again.

use geode_core::vol::Coordinate;

use crate::core::model::{Kind, Pair};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Underlying(String),
    X(Coordinate),
    Diff(Diff),
    /// The differences axis's fixed y domain; `None` autoscales it.
    Ylim(Option<(f64, f64)>),
}

/// What `:diff` asks of the shown differences.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Diff {
    /// Turn the pair on, or off when it is on.
    Toggle(Pair),
    /// Turn every pair off.
    Off,
}

/// Completion verbs, in the order offered.
const VERBS: [&str; 4] = ["underlying", "x", "diff", "ylim"];

const YLIM_USAGE: &str = "usage: :ylim <lo> <hi> | off";

/// The words that autoscale the differences axis again.
const YLIM_OFF: [&str; 2] = ["off", "auto"];

const DIFF_USAGE: &str = "usage: :diff <kind> - <kind> | off";

/// The word that clears every pair, and the older one read the same way.
const OFF: [&str; 2] = ["off", "none"];

/// The two minus spellings a difference is typed with: an ASCII hyphen
/// between spaces (a kind label holds a space, `cvi draft`, so a bare
/// space cannot part the kinds), and the label's own `\u{2212}`.
const MINUS: [&str; 2] = [" - ", "\u{2212}"];

fn is_minus(word: &str) -> bool {
    word == "-" || word == "\u{2212}"
}

fn kind(text: &str) -> Result<Kind, String> {
    Kind::parse(text).ok_or_else(|| format!("unknown kind '{}'", text.trim()))
}

fn parse_diff(rest: &str) -> Result<Command, String> {
    let rest = rest.trim();
    if OFF.contains(&rest) {
        return Ok(Command::Diff(Diff::Off));
    }
    let Some((a, b)) = MINUS.iter().find_map(|m| rest.split_once(m)) else {
        return Err(DIFF_USAGE.into());
    };
    if a.trim().is_empty() || b.trim().is_empty() {
        return Err(DIFF_USAGE.into());
    }
    let (a, b) = (kind(a)?, kind(b)?);
    Pair::new(a, b)
        .map(|p| Command::Diff(Diff::Toggle(p)))
        .ok_or_else(|| "a difference needs two different kinds".into())
}

/// One `:ylim` value: a number in the axis's units, or a percent.
fn ylim_value(word: &str) -> Result<f64, String> {
    let (digits, scale) = match word.strip_suffix('%') {
        Some(d) => (d, 0.01),
        None => (word, 1.0),
    };
    // `\u{2212}`, the minus the labels print, reads as a hyphen.
    let digits = digits.replace('\u{2212}', "-");
    digits
        .parse::<f64>()
        .ok()
        .map(|v| v * scale)
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("'{word}' is not a finite number"))
}

fn parse_ylim(rest: &str) -> Result<Command, String> {
    if YLIM_OFF.contains(&rest) {
        return Ok(Command::Ylim(None));
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let [lo, hi] = words[..] else {
        return Err(YLIM_USAGE.into());
    };
    let (lo, hi) = (ylim_value(lo)?, ylim_value(hi)?);
    if lo >= hi {
        return Err("the lower limit must be below the upper".into());
    }
    Ok(Command::Ylim(Some((lo, hi))))
}

/// Parse a line without its leading colon.
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    match verb {
        "underlying" => {
            let mut words = rest.split_whitespace();
            match (words.next(), words.next()) {
                (Some(u), None) => Ok(Command::Underlying(u.to_string())),
                _ => Err("usage: :underlying <ref>".into()),
            }
        }
        "x" => {
            if rest.is_empty() {
                return Err("usage: :x <strike|moneyness|log-moneyness|delta>".into());
            }
            Coordinate::parse(rest)
                .map(Command::X)
                .ok_or_else(|| format!("unknown coordinate '{rest}'"))
        }
        "diff" => parse_diff(rest),
        "ylim" => parse_ylim(rest),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// Unranked candidates for the word under `cursor` (a byte offset): the
/// verbs, then coordinate names after `x`, then the loaded kinds' labels
/// (and `off` as the first word) after `diff`.
pub fn completions(line: &str, cursor: usize, kinds: &[Kind]) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut words: Vec<&str> = line[..cursor].split(char::is_whitespace).collect();
    // The word under the cursor is the shell's to rank; the words before
    // it decide which position is being completed.
    words.pop();
    words.retain(|w| !w.is_empty());
    let labels = || kinds.iter().map(|k| k.label().to_string());
    match words.as_slice() {
        [] => VERBS.iter().map(|v| (*v).to_string()).collect(),
        ["x"] => Coordinate::ALL
            .iter()
            .map(|c| c.name().to_string())
            .collect(),
        ["diff"] => labels().chain([OFF[0].to_string()]).collect(),
        ["diff", .., last] if is_minus(last) => labels().collect(),
        ["ylim"] => vec![YLIM_OFF[0].to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_every_command_and_both_minus_spellings() {
        assert_eq!(
            parse("underlying SPX.Z"),
            Ok(Command::Underlying("SPX.Z".into()))
        );
        assert_eq!(parse("x strike"), Ok(Command::X(Coordinate::Strike)));
        assert_eq!(parse("x log"), Ok(Command::X(Coordinate::LogMoneyness)));
        assert_eq!(
            parse("x log-moneyness"),
            Ok(Command::X(Coordinate::LogMoneyness))
        );
        assert_eq!(parse("x delta"), Ok(Command::X(Coordinate::Delta)));
        let draft_cvi = Command::Diff(Diff::Toggle(Pair::new(Kind::Draft, Kind::Cvi).unwrap()));
        assert_eq!(parse("diff cvi draft - cvi"), Ok(draft_cvi.clone()));
        assert_eq!(
            parse("diff cvi draft \u{2212} cvi"),
            Ok(draft_cvi),
            "the label's own minus"
        );
        assert_eq!(
            parse("diff  chain   -  cvi "),
            Ok(Command::Diff(Diff::Toggle(
                Pair::new(Kind::Chain, Kind::Cvi).unwrap()
            )))
        );
        assert_eq!(parse("diff off"), Ok(Command::Diff(Diff::Off)));
        assert_eq!(
            parse("diff none"),
            Ok(Command::Diff(Diff::Off)),
            "the older word"
        );
    }

    #[test]
    fn parse_refuses_with_the_documented_words() {
        let err = |l: &str| parse(l).unwrap_err();
        assert_eq!(err("x foo"), "unknown coordinate 'foo'");
        assert_eq!(err("diff foo - cvi"), "unknown kind 'foo'");
        assert_eq!(
            err("diff cvi - cvi"),
            "a difference needs two different kinds"
        );
        assert_eq!(err("diff cvi"), "usage: :diff <kind> - <kind> | off");
        assert_eq!(err("diff"), "usage: :diff <kind> - <kind> | off");
        assert_eq!(err("foo bar"), "unknown command 'foo'");
        assert!(err("underlying").starts_with("usage: :underlying"));
        assert!(err("x").starts_with("usage: :x"));
    }

    #[test]
    fn ylim_reads_units_percents_and_off() {
        assert_eq!(
            parse("ylim -0.02 0.02"),
            Ok(Command::Ylim(Some((-0.02, 0.02))))
        );
        assert_eq!(parse("ylim -2% 2%"), Ok(Command::Ylim(Some((-0.02, 0.02)))));
        assert_eq!(
            parse("ylim \u{2212}1.5% 0.03"),
            Ok(Command::Ylim(Some((-0.015, 0.03)))),
            "a percent beside a plain value, and the label's minus"
        );
        assert_eq!(parse("ylim off"), Ok(Command::Ylim(None)));
        assert_eq!(parse("ylim auto"), Ok(Command::Ylim(None)));
    }

    #[test]
    fn ylim_refuses_a_range_that_is_not_one() {
        let err = |l: &str| parse(l).unwrap_err();
        let order = "the lower limit must be below the upper";
        assert_eq!(err("ylim 0.02 -0.02"), order);
        assert_eq!(err("ylim 2% 2%"), order, "an empty range");
        assert_eq!(err("ylim nan 1"), "'nan' is not a finite number");
        assert_eq!(err("ylim -inf 1"), "'-inf' is not a finite number");
        assert_eq!(err("ylim 1 two"), "'two' is not a finite number");
        assert_eq!(err("ylim"), "usage: :ylim <lo> <hi> | off");
        assert_eq!(err("ylim 0.02"), "usage: :ylim <lo> <hi> | off");
        assert_eq!(err("ylim 1 2 3"), "usage: :ylim <lo> <hi> | off");
    }

    #[test]
    fn completions_offer_verbs_then_their_arguments() {
        let kinds = [Kind::Cvi, Kind::Draft, Kind::Chain];
        let verbs = ["underlying", "x", "diff", "ylim"];
        assert_eq!(completions("", 0, &kinds), verbs);
        assert_eq!(completions("di", 2, &kinds), verbs);
        assert_eq!(completions("ylim ", 5, &kinds), ["off"]);
        assert!(completions("ylim -2% ", 9, &kinds).is_empty());
        assert_eq!(
            completions("x ", 2, &kinds),
            ["strike", "moneyness", "log-moneyness", "delta"]
        );
        assert_eq!(
            completions("diff ", 5, &kinds),
            ["cvi", "cvi draft", "chain", "off"]
        );
        assert_eq!(
            completions("diff cvi - ", 11, &kinds[..2]),
            ["cvi", "cvi draft"],
            "the subtrahend is a kind, never off"
        );
        assert!(completions("underlying ", 11, &kinds).is_empty());
        assert!(completions("x delta ", 8, &kinds).is_empty());
    }
}
