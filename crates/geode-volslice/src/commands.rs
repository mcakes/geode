//! Pure parsing and completion vocabulary for the tile's `:` line.
//! Commands come back as data; the tile owns every change they make.
//!
//! Vocabulary: `underlying <ref>`, `x <coordinate>` and
//! `diff <kind> - <kind> | none`.

use geode_core::vol::Coordinate;

use crate::core::model::{Kind, Pair};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Underlying(String),
    X(Coordinate),
    Diff(Option<Pair>),
}

/// Completion verbs, in the order offered.
const VERBS: [&str; 3] = ["underlying", "x", "diff"];

const DIFF_USAGE: &str = "usage: :diff <kind> - <kind> | none";

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
    if rest == "none" {
        return Ok(Command::Diff(None));
    }
    let Some((a, b)) = MINUS.iter().find_map(|m| rest.split_once(m)) else {
        return Err(DIFF_USAGE.into());
    };
    if a.trim().is_empty() || b.trim().is_empty() {
        return Err(DIFF_USAGE.into());
    }
    let (a, b) = (kind(a)?, kind(b)?);
    Pair::new(a, b)
        .map(|p| Command::Diff(Some(p)))
        .ok_or_else(|| "a difference needs two different kinds".into())
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
        other => Err(format!("unknown command '{other}'")),
    }
}

/// Unranked candidates for the word under `cursor` (a byte offset): the
/// verbs, then coordinate names after `x`, then the loaded kinds' labels
/// (and `none` as the first word) after `diff`.
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
        ["diff"] => labels().chain(["none".to_string()]).collect(),
        ["diff", .., last] if is_minus(last) => labels().collect(),
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
        let draft_cvi = Pair::new(Kind::Draft, Kind::Cvi);
        assert_eq!(parse("diff cvi draft - cvi"), Ok(Command::Diff(draft_cvi)));
        assert_eq!(
            parse("diff cvi draft \u{2212} cvi"),
            Ok(Command::Diff(draft_cvi)),
            "the label's own minus"
        );
        assert_eq!(
            parse("diff  chain   -  cvi "),
            Ok(Command::Diff(Pair::new(Kind::Chain, Kind::Cvi)))
        );
        assert_eq!(parse("diff none"), Ok(Command::Diff(None)));
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
        assert_eq!(err("diff cvi"), "usage: :diff <kind> - <kind> | none");
        assert_eq!(err("diff"), "usage: :diff <kind> - <kind> | none");
        assert_eq!(err("foo bar"), "unknown command 'foo'");
        assert!(err("underlying").starts_with("usage: :underlying"));
        assert!(err("x").starts_with("usage: :x"));
    }

    #[test]
    fn completions_offer_verbs_then_their_arguments() {
        let kinds = [Kind::Cvi, Kind::Draft, Kind::Chain];
        assert_eq!(completions("", 0, &kinds), ["underlying", "x", "diff"]);
        assert_eq!(completions("di", 2, &kinds), ["underlying", "x", "diff"]);
        assert_eq!(
            completions("x ", 2, &kinds),
            ["strike", "moneyness", "log-moneyness", "delta"]
        );
        assert_eq!(
            completions("diff ", 5, &kinds),
            ["cvi", "cvi draft", "chain", "none"]
        );
        assert_eq!(
            completions("diff cvi - ", 11, &kinds[..2]),
            ["cvi", "cvi draft"],
            "the subtrahend is a kind, never none"
        );
        assert!(completions("underlying ", 11, &kinds).is_empty());
        assert!(completions("x delta ", 8, &kinds).is_empty());
    }
}
