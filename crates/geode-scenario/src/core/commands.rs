//! The tile's `:` grammar: `filter`, `unscoped`, `asof` and `scale`, parsed
//! without the colon, and the completion vocabulary for each word. Frame-
//! wide verbs (`scope`, `live`, `asof undo`) are refused by name so the
//! error names the palette entry that owns them.

use geode_core::view::Scale;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOfArg {
    At(String),
    Live,
    Clear,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    FilterExpr(String),
    FilterText(String),
    FilterClear,
    Unscoped,
    AsOf(AsOfArg),
    /// `None` restores each column's own scale (`:scale 1`).
    Scale(Option<Scale>),
    Refused(&'static str),
}

/// The words the first position completes to, sorted.
pub const COMMANDS: [&str; 4] = ["asof", "filter", "scale", "unscoped"];

pub const REFUSED_SCOPE: &str = ":scope is frame-wide — the scope bar (mod+/), Set scope expression…, or the palette's Scope: entries";
pub const REFUSED_ASOF_UNDO: &str =
    "frame as-of undo is in the palette (Swap to the previous as of)";
pub const REFUSED_LIVE: &str = ":asof live pins this tile; Return to live (palette) sets the frame";

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    match word {
        "" => Err("a command is needed: filter, unscoped, asof or scale".into()),
        "unscoped" => Ok(Command::Unscoped),
        "live" => Ok(Command::Refused(REFUSED_LIVE)),
        "scope" => Ok(Command::Refused(REFUSED_SCOPE)),
        "filter" => {
            let mut words = rest.splitn(2, char::is_whitespace);
            match (words.next(), words.next().map(str::trim)) {
                (Some("clear"), None) => Ok(Command::FilterClear),
                (Some("text"), second) => {
                    Ok(Command::FilterText(second.unwrap_or("").to_string()))
                }
                (Some(""), _) => Err("filter needs an expression, `text …` or `clear`".into()),
                _ => Ok(Command::FilterExpr(rest.to_string())),
            }
        }
        "asof" => match rest {
            "" => Err("asof needs a time (HH:MM, HH:MM:SS, YYYY-MM-DD[ HH:MM[:SS]] or RFC 3339), `live` or `clear`".into()),
            "undo" => Ok(Command::Refused(REFUSED_ASOF_UNDO)),
            "live" => Ok(Command::AsOf(AsOfArg::Live)),
            "clear" => Ok(Command::AsOf(AsOfArg::Clear)),
            t => Ok(Command::AsOf(AsOfArg::At(t.to_string()))),
        },
        "scale" => match rest {
            "k" => Ok(Command::Scale(Some(Scale::Thousands))),
            "m" => Ok(Command::Scale(Some(Scale::Millions))),
            "1" => Ok(Command::Scale(None)),
            _ => Err("scale takes k, m or 1".into()),
        },
        other => Err(format!("unknown command '{other}'")),
    }
}

/// What `filter` completes over: the dataset's groupable dimensions and
/// the shape's measure columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vocabulary {
    pub dimensions: Vec<String>,
    pub columns: Vec<String>,
}

/// The candidates for the word at `cursor`, sorted and deduplicated; the
/// shell ranks them against the typed word. A cursor inside a multibyte
/// character steps back to that character's start.
pub fn completions(line: &str, cursor: usize, vocab: &Vocabulary) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &line[..cursor];
    let mut words: Vec<&str> = before.split_whitespace().collect();
    // The word being typed is not context: drop it unless the cursor sits
    // after whitespace (then every word before it is context).
    if !before.ends_with(char::is_whitespace) {
        words.pop();
    }
    let mut out: Vec<String> = match words.as_slice() {
        [] => COMMANDS.iter().map(|s| s.to_string()).collect(),
        ["filter"] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v.extend(["clear", "text"].map(String::from));
            v
        }
        ["filter", "text", ..] => Vec::new(),
        ["filter", ..] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v
        }
        ["asof"] => vec!["clear".into(), "live".into()],
        ["scale"] => vec!["1".into(), "k".into(), "m".into()],
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocabulary {
        Vocabulary {
            dimensions: vec!["book".into(), "underlying_ref".into()],
            columns: vec!["vega".into()],
        }
    }

    #[test]
    fn every_command_parses_to_its_variant() {
        assert_eq!(parse("unscoped"), Ok(Command::Unscoped));
        assert_eq!(parse("filter clear"), Ok(Command::FilterClear));
        assert_eq!(
            parse("filter text SPX"),
            Ok(Command::FilterText("SPX".into()))
        );
        assert_eq!(
            parse("filter vega > 0"),
            Ok(Command::FilterExpr("vega > 0".into()))
        );
        assert_eq!(parse("asof live"), Ok(Command::AsOf(AsOfArg::Live)));
        assert_eq!(parse("asof clear"), Ok(Command::AsOf(AsOfArg::Clear)));
        assert_eq!(
            parse("asof 10:30"),
            Ok(Command::AsOf(AsOfArg::At("10:30".into())))
        );
        assert_eq!(parse("scale k"), Ok(Command::Scale(Some(Scale::Thousands))));
        assert_eq!(parse("scale m"), Ok(Command::Scale(Some(Scale::Millions))));
        assert_eq!(parse("scale 1"), Ok(Command::Scale(None)));
    }

    #[test]
    fn frame_wide_verbs_are_refused_by_name() {
        assert_eq!(parse("scope book = A"), Ok(Command::Refused(REFUSED_SCOPE)));
        assert_eq!(parse("live"), Ok(Command::Refused(REFUSED_LIVE)));
        assert_eq!(parse("asof undo"), Ok(Command::Refused(REFUSED_ASOF_UNDO)));
    }

    #[test]
    fn bad_lines_say_what_they_need() {
        assert!(parse("").unwrap_err().contains("command"));
        assert!(parse("filter").unwrap_err().contains("expression"));
        assert!(parse("asof").unwrap_err().contains("time"));
        assert!(parse("scale").unwrap_err().contains("k, m or 1"));
        assert!(parse("scale x").unwrap_err().contains("k, m or 1"));
        assert!(parse("sort vega").unwrap_err().contains("unknown command"));
    }

    #[test]
    fn completions_follow_the_word_under_the_cursor() {
        let v = vocab();
        assert_eq!(
            completions("", 0, &v),
            ["asof", "filter", "scale", "unscoped"]
        );
        assert_eq!(
            completions("fi", 2, &v),
            ["asof", "filter", "scale", "unscoped"]
        );
        assert_eq!(
            completions("filter ", 7, &v),
            ["book", "clear", "text", "underlying_ref", "vega"]
        );
        assert_eq!(completions("filter text ", 12, &v), Vec::<String>::new());
        assert_eq!(
            completions("filter book ", 12, &v),
            ["book", "underlying_ref", "vega"]
        );
        assert_eq!(completions("asof ", 5, &v), ["clear", "live"]);
        assert_eq!(completions("scale ", 6, &v), ["1", "k", "m"]);
        assert_eq!(completions("unscoped ", 9, &v), Vec::<String>::new());
    }

    #[test]
    fn a_cursor_inside_a_multibyte_word_does_not_panic() {
        let v = vocab();
        let line = "filter ≡";
        assert_eq!(
            completions(line, line.len() - 1, &v),
            ["book", "clear", "text", "underlying_ref", "vega"]
        );
    }
}
