//! Pure parsing and completion for the tile's `:` commands. `:e`, `:new`,
//! and `:name` change this tile's sheet; `:rm` removes a stored sheet only
//! when no tile holds it. No command changes another tile's sheet.

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
    /// `:package [count]` (`g p`): the cursor line and the next
    /// `count − 1` roots become a custom package.
    Package(Option<usize>),
    /// `:unpackage` (`g u`): dissolve the cursor's package.
    Unpackage,
    /// `:group <cols…>`: pin this tile to a grouping chain (the
    /// blotter's `Pin::Grouping`); columns split on commas or spaces.
    Group(Vec<String>),
    /// `:group slot N`: pin this tile to frame slot `N` (1–9).
    GroupSlot(u8),
    /// `:unpin`: follow the frame's grouping again.
    Unpin,
    /// `:e <sheet>`: open another sheet in this tile.
    Edit(String),
    /// `:new [sheet]`: an empty sheet under that name, else the next
    /// `untitled-N`.
    New(Option<String>),
    /// `:name <sheet>`: rename this tile's sheet.
    Name(String),
    /// `:rm <sheet>`: remove a sheet no tile holds (asks first).
    Remove(String),
    /// `:autosize [reset]`: fit every column to its content, or return to
    /// the view's widths.
    Autosize {
        reset: bool,
    },
    /// `:unscoped`: toggle whether this tile ignores the frame's scope.
    Unscoped,
}

pub const VERBS: [&str; 15] = [
    "view",
    "shift",
    "spot",
    "price",
    "refresh",
    "package",
    "unpackage",
    "group",
    "unpin",
    "e",
    "new",
    "name",
    "rm",
    "autosize",
    "unscoped",
];

const SHIFT_USAGE: &str = "usage: shift spot|vol <n>|clear";
const SPOT_USAGE: &str = "usage: spot <underlying> <level>|clear";
const REFRESH_USAGE: &str = "usage: refresh <duration>|off|default";
const GROUP_SLOT_USAGE: &str = "group slot needs a slot number 1–9";

/// A sheet name is one word (the line splits on whitespace) with no
/// control character: the document key joins on `U+001F`, so a name
/// holding it would address a different document than the one named.
fn sheet_name(verb: &str, name: &str) -> Result<String, String> {
    if name.chars().any(char::is_control) {
        return Err(format!(
            ":{verb}: a sheet name cannot hold a control character"
        ));
    }
    Ok(name.to_string())
}

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
        ["package"] => Ok(Command::Package(None)),
        ["package", n] => n
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(|n| Command::Package(Some(n)))
            .ok_or_else(|| "usage: package [count]".into()),
        ["package", ..] => Err("usage: package [count]".into()),
        ["unpackage"] => Ok(Command::Unpackage),
        ["unpackage", ..] => Err("usage: unpackage".into()),
        // The blotter's `:group`: a bare one names nothing to pin.
        ["group"] => Err("group needs columns or `slot N`".into()),
        ["group", "slot", rest @ ..] => match rest {
            [n] => n
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=9).contains(n))
                .map(Command::GroupSlot)
                .ok_or_else(|| GROUP_SLOT_USAGE.into()),
            _ => Err(GROUP_SLOT_USAGE.into()),
        },
        ["group", ..] => Ok(Command::Group(
            words[1..]
                .iter()
                .flat_map(|w| w.split(','))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        ["unpin"] => Ok(Command::Unpin),
        ["unpin", ..] => Err("usage: unpin".into()),
        ["e", name] => sheet_name("e", name).map(Command::Edit),
        ["e", ..] => Err("usage: e <sheet>".into()),
        ["new"] => Ok(Command::New(None)),
        ["new", name] => sheet_name("new", name).map(|n| Command::New(Some(n))),
        ["new", ..] => Err("usage: new [sheet]".into()),
        ["name", name] => sheet_name("name", name).map(Command::Name),
        ["name", ..] => Err("usage: name <sheet>".into()),
        ["rm", name] => sheet_name("rm", name).map(Command::Remove),
        ["rm", ..] => Err("usage: rm <sheet>".into()),
        ["autosize"] => Ok(Command::Autosize { reset: false }),
        ["autosize", "reset"] => Ok(Command::Autosize { reset: true }),
        ["autosize", ..] => Err("usage: autosize [reset]".into()),
        ["unscoped"] => Ok(Command::Unscoped),
        ["unscoped", ..] => Err("usage: unscoped".into()),
        [other, ..] => Err(format!("unknown command '{other}'")),
    }
}

/// The bare words valid at `cursor` — the position's whole vocabulary,
/// unfiltered; the shell ranks (`TileContent::completions`). `sheets`
/// is the known sheet names, for `:e` and `:rm`; `:name` takes a new
/// name, so it offers none. `groupable` is what `:group` can pin: the
/// `pricer` dataset's groupable columns and the derived dimensions over
/// them.
pub fn completions(
    line: &str,
    cursor: usize,
    views: &[String],
    underlyings: &[String],
    sheets: &[String],
    groupable: &[String],
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
        ["e"] | ["rm"] => sheets.to_vec(),
        ["autosize"] => strs(&["reset"]),
        ["group"] => groupable
            .iter()
            .cloned()
            .chain(["slot".to_string()])
            .collect(),
        ["group", "slot"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => groupable.to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn autosize_parses_an_optional_reset_and_completes_it() {
        assert_eq!(parse("autosize"), Ok(Command::Autosize { reset: false }));
        assert_eq!(
            parse("autosize reset"),
            Ok(Command::Autosize { reset: true })
        );
        assert!(parse("autosize wide").is_err());
        assert_eq!(
            completions("autosize ", 9, &[], &[], &[], &[]),
            vec!["reset"]
        );
    }

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
        assert_eq!(parse("package"), Ok(Command::Package(None)));
        assert_eq!(parse("package 3"), Ok(Command::Package(Some(3))));
        assert_eq!(parse("unpackage"), Ok(Command::Unpackage));
        assert_eq!(
            parse("group underlying_ref, expiry"),
            Ok(Command::Group(vec![
                "underlying_ref".into(),
                "expiry".into()
            ]))
        );
        assert_eq!(parse("group slot 2"), Ok(Command::GroupSlot(2)));
        assert_eq!(parse("unpin"), Ok(Command::Unpin));
        assert_eq!(parse("e book"), Ok(Command::Edit("book".into())));
        assert_eq!(parse("new"), Ok(Command::New(None)));
        assert_eq!(parse("new fresh"), Ok(Command::New(Some("fresh".into()))));
        assert_eq!(parse("name fresh"), Ok(Command::Name("fresh".into())));
        assert_eq!(parse("rm old"), Ok(Command::Remove("old".into())));
        assert_eq!(parse("unscoped"), Ok(Command::Unscoped));
        assert_eq!(parse("unscoped now"), Err("usage: unscoped".into()));
    }

    #[test]
    fn bad_arguments_answer_the_usage() {
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
        assert_eq!(parse("package 0"), Err("usage: package [count]".into()));
        assert_eq!(parse("unpackage 2"), Err("usage: unpackage".into()));
        // The retired package verbs are gone, not aliased: `:group 2`
        // pins a grouping by a column named `2` (which `pricer` lacks, so
        // the tile drops it and the header strikes it through), never
        // packages two lines.
        assert_eq!(parse("group 2"), Ok(Command::Group(vec!["2".into()])));
        assert_eq!(parse("ungroup"), Err("unknown command 'ungroup'".into()));
        assert_eq!(
            parse("group"),
            Err("group needs columns or `slot N`".into())
        );
        assert_eq!(
            parse("group slot 0"),
            Err("group slot needs a slot number 1–9".into())
        );
        assert_eq!(
            parse("group slot"),
            Err("group slot needs a slot number 1–9".into())
        );
        assert_eq!(parse("unpin now"), Err("usage: unpin".into()));
        assert_eq!(parse("price now"), Err("usage: price".into()));
        assert_eq!(parse("e"), Err("usage: e <sheet>".into()));
        assert_eq!(parse("e a b"), Err("usage: e <sheet>".into()));
        assert_eq!(parse("new a b"), Err("usage: new [sheet]".into()));
        assert_eq!(
            parse("new a\u{1f}b"),
            Err(":new: a sheet name cannot hold a control character".into())
        );
        assert_eq!(parse("name"), Err("usage: name <sheet>".into()));
        assert_eq!(parse("rm"), Err("usage: rm <sheet>".into()));
        assert_eq!(
            parse("e a\u{1f}b"),
            Err(":e: a sheet name cannot hold a control character".into())
        );
        assert_eq!(parse("bogus"), Err("unknown command 'bogus'".into()));
    }

    #[test]
    fn completions_offer_each_positions_vocabulary_unfiltered() {
        let views = vec!["vanilla".to_string(), "barrier".to_string()];
        let unds = vec!["NDX".to_string(), "SPX".to_string()];
        let sheets = vec!["alpha".to_string(), "book".to_string()];
        let groupable = vec!["expiry".to_string(), "underlying_ref".to_string()];
        let completions = |line: &str, cursor, views: &[String], unds: &[String]| {
            completions(line, cursor, views, unds, &sheets, &groupable)
        };
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
        assert_eq!(completions("e ", 2, &views, &unds), sheets);
        assert_eq!(completions("rm ", 3, &views, &unds), sheets);
        assert!(completions("name ", 5, &views, &unds).is_empty());
        assert!(completions("new ", 4, &views, &unds).is_empty());
        assert_eq!(
            completions("group ", 6, &views, &unds),
            vec!["expiry", "underlying_ref", "slot"]
        );
        assert_eq!(
            completions("group expiry ", 13, &views, &unds),
            vec!["expiry", "underlying_ref"]
        );
        assert_eq!(
            completions("group slot ", 11, &views, &unds),
            (1..=9).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        assert!(completions("package ", 8, &views, &unds).is_empty());
        assert!(completions("unpin ", 6, &views, &unds).is_empty());
    }

    #[test]
    fn a_cursor_off_a_char_boundary_does_not_panic() {
        let _ = completions("spot é", 6, &[], &[], &[], &[]);
        let _ = completions("spot é", 99, &[], &[], &[], &[]);
    }
}
