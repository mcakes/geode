//! The tile's `:` vocabulary (line-pricer spec §8.6), pure: parse and
//! completion. Every verb changes only this tile. `:e`, `:name`, `:new`
//! and `:rm` are Part 4's; they parse to a refusal that names them
//! (Part 3 planning decision 9) rather than "unknown command".

use crate::core::sheet::Refresh;
use geode_core::source_config::parse_duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftField {
    Spot,
    Vol,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    View(String),
    /// `value: None` clears the sheet-wide value.
    Shift {
        field: ShiftField,
        value: Option<f64>,
    },
    /// `(Some(u), Some(level))` sets; `(Some(u), None)` clears one;
    /// `(None, None)` clears every override.
    Spot {
        underlying: Option<String>,
        level: Option<f64>,
    },
    Price,
    Refresh(Refresh),
    Group(Option<usize>),
    Ungroup,
}

pub const VERBS: [&str; 7] = [
    "view", "shift", "spot", "price", "refresh", "group", "ungroup",
];
pub const NOT_BUILT: [&str; 4] = ["e", "name", "new", "rm"];

const SHIFT_USAGE: &str = "usage: shift spot|vol <n>|clear";
const SPOT_USAGE: &str = "usage: spot <underlying> <level>|clear";
const REFRESH_USAGE: &str = "usage: refresh <duration>|off|default";

pub fn parse(line: &str) -> Result<Command, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [] => Err("empty command".into()),
        ["view", name] => Ok(Command::View((*name).to_string())),
        ["view", ..] => Err("usage: view <name>".into()),
        ["shift", field, value] => {
            let field = match *field {
                "spot" => ShiftField::Spot,
                "vol" => ShiftField::Vol,
                _ => return Err(SHIFT_USAGE.into()),
            };
            let value = match *value {
                "clear" => None,
                v => Some(
                    v.parse::<f64>()
                        .ok()
                        .filter(|x| x.is_finite())
                        .ok_or_else(|| format!("shift: '{v}' is not a number"))?,
                ),
            };
            Ok(Command::Shift { field, value })
        }
        ["shift", ..] => Err(SHIFT_USAGE.into()),
        ["spot", "clear"] => Ok(Command::Spot {
            underlying: None,
            level: None,
        }),
        ["spot", und, "clear"] => Ok(Command::Spot {
            underlying: Some(und.to_ascii_uppercase()),
            level: None,
        }),
        ["spot", und, level] => {
            let level = level
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite() && *x > 0.0)
                .ok_or_else(|| format!("spot: '{und} {level}' needs a positive level"))?;
            Ok(Command::Spot {
                underlying: Some(und.to_ascii_uppercase()),
                level: Some(level),
            })
        }
        ["spot", ..] => Err(SPOT_USAGE.into()),
        ["price"] => Ok(Command::Price),
        ["price", ..] => Err("usage: price".into()),
        ["refresh", "off"] => Ok(Command::Refresh(Refresh::Off)),
        ["refresh", "default"] => Ok(Command::Refresh(Refresh::Default)),
        ["refresh", d] => parse_duration(d)
            .filter(|d| !d.is_zero())
            .map(|d| Command::Refresh(Refresh::Every(d)))
            .ok_or_else(|| REFRESH_USAGE.into()),
        ["refresh", ..] => Err(REFRESH_USAGE.into()),
        ["group"] => Ok(Command::Group(None)),
        ["group", n] => n
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(|n| Command::Group(Some(n)))
            .ok_or_else(|| "usage: group [count]".into()),
        ["group", ..] => Err("usage: group [count]".into()),
        ["ungroup"] => Ok(Command::Ungroup),
        ["ungroup", ..] => Err("usage: ungroup".into()),
        [verb, ..] if NOT_BUILT.contains(verb) => Err(format!(":{verb} is not built yet")),
        [other, ..] => Err(format!("unknown command '{other}'")),
    }
}

/// The bare words valid at `cursor` — the position's whole vocabulary,
/// unfiltered; the shell ranks (`TileContent::completions`).
pub fn completions(
    line: &str,
    cursor: usize,
    views: &[String],
    underlyings: &[String],
) -> Vec<String> {
    let mut end = cursor.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let mut words: Vec<&str> = line[..end].split(char::is_whitespace).collect();
    words.pop(); // the word under the cursor
    words.retain(|w| !w.is_empty());
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match words.as_slice() {
        [] => strs(&VERBS),
        ["view"] => views.to_vec(),
        ["shift"] => strs(&["spot", "vol"]),
        ["shift", _] => strs(&["clear"]),
        ["spot"] => underlyings
            .iter()
            .cloned()
            .chain(["clear".to_string()])
            .collect(),
        ["spot", _] => strs(&["clear"]),
        ["refresh"] => strs(&["off", "default"]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_verb_parses_its_arguments() {
        assert_eq!(parse("view barrier"), Ok(Command::View("barrier".into())));
        assert_eq!(
            parse("shift spot 2"),
            Ok(Command::Shift {
                field: ShiftField::Spot,
                value: Some(2.0)
            })
        );
        assert_eq!(
            parse("shift vol -1.5"),
            Ok(Command::Shift {
                field: ShiftField::Vol,
                value: Some(-1.5)
            })
        );
        assert_eq!(
            parse("shift vol clear"),
            Ok(Command::Shift {
                field: ShiftField::Vol,
                value: None
            })
        );
        assert_eq!(
            parse("spot spx 5100"),
            Ok(Command::Spot {
                underlying: Some("SPX".into()),
                level: Some(5100.0)
            })
        );
        assert_eq!(
            parse("spot SPX clear"),
            Ok(Command::Spot {
                underlying: Some("SPX".into()),
                level: None
            })
        );
        assert_eq!(
            parse("spot clear"),
            Ok(Command::Spot {
                underlying: None,
                level: None
            })
        );
        assert_eq!(parse("price"), Ok(Command::Price));
        assert_eq!(parse("refresh off"), Ok(Command::Refresh(Refresh::Off)));
        assert_eq!(
            parse("refresh default"),
            Ok(Command::Refresh(Refresh::Default))
        );
        assert_eq!(
            parse("refresh 500ms"),
            Ok(Command::Refresh(Refresh::Every(Duration::from_millis(500))))
        );
        assert_eq!(parse("group"), Ok(Command::Group(None)));
        assert_eq!(parse("group 3"), Ok(Command::Group(Some(3))));
        assert_eq!(parse("ungroup"), Ok(Command::Ungroup));
    }

    #[test]
    fn bad_arguments_answer_the_usage_and_part_4_verbs_refuse_by_name() {
        assert_eq!(parse(""), Err("empty command".into()));
        assert_eq!(parse("view"), Err("usage: view <name>".into()));
        assert_eq!(
            parse("shift up 2"),
            Err("usage: shift spot|vol <n>|clear".into())
        );
        assert_eq!(
            parse("shift spot x"),
            Err("shift: 'x' is not a number".into())
        );
        assert_eq!(
            parse("spot SPX -1"),
            Err("spot: 'SPX -1' needs a positive level".into())
        );
        assert_eq!(
            parse("spot"),
            Err("usage: spot <underlying> <level>|clear".into())
        );
        assert_eq!(
            parse("refresh 0s"),
            Err("usage: refresh <duration>|off|default".into())
        );
        assert_eq!(
            parse("refresh soon"),
            Err("usage: refresh <duration>|off|default".into())
        );
        assert_eq!(parse("group 0"), Err("usage: group [count]".into()));
        assert_eq!(parse("price now"), Err("usage: price".into()));
        for verb in NOT_BUILT {
            assert_eq!(parse(verb), Err(format!(":{verb} is not built yet")));
        }
        assert_eq!(parse("bogus"), Err("unknown command 'bogus'".into()));
    }

    #[test]
    fn completions_offer_each_positions_vocabulary_unfiltered() {
        let views = vec!["vanilla".to_string(), "barrier".to_string()];
        let unds = vec!["NDX".to_string(), "SPX".to_string()];
        assert_eq!(
            completions("", 0, &views, &unds),
            VERBS.map(String::from).to_vec()
        );
        assert_eq!(
            completions("vi", 2, &views, &unds),
            VERBS.map(String::from).to_vec(),
            "the shell ranks"
        );
        assert_eq!(completions("view ", 5, &views, &unds), views);
        assert_eq!(completions("shift ", 6, &views, &unds), vec!["spot", "vol"]);
        assert_eq!(completions("shift spot ", 11, &views, &unds), vec!["clear"]);
        assert_eq!(
            completions("spot ", 5, &views, &unds),
            vec!["NDX", "SPX", "clear"]
        );
        assert_eq!(completions("spot SPX ", 9, &views, &unds), vec!["clear"]);
        assert_eq!(
            completions("refresh ", 8, &views, &unds),
            vec!["off", "default"]
        );
        assert!(completions("price ", 6, &views, &unds).is_empty());
    }

    #[test]
    fn a_cursor_off_a_char_boundary_does_not_panic() {
        let _ = completions("spot é", 6, &[], &[]);
        let _ = completions("spot é", 99, &[], &[]);
    }
}
