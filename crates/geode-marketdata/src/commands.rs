//! Pure parsing and completion vocabulary for the panel's tile-local `:` line.
//! Commands are returned as data; the tile owns mutation, focus, and I/O.
//!
//! Vocabulary: `underlying <value>` (`key` is an unlisted alias), `revert`,
//! `bump <delta> [row|col]`, `rebase`, `upload [target]`, `set <attr> [value...]`,
//! `auto [hold|rebase|replace]`, `menu`, and `autosize [reset]`. Upload arms confirmation rather than
//! sending immediately. Completion returns candidates for the shell to rank.

use crate::core::UpdatePolicy;
use geode_core::document::KEY_SEPARATOR;

/// The separator a multi-part document key is typed and displayed with.
///
/// A document's storage identity joins its key parts with
/// [`KEY_SEPARATOR`] (`\u{1f}`), which is deliberately untypeable — so
/// the panel's own spelling of the same key is `/`-separated, matching
/// the completions the catalog offers ([`crate::tile`]'s
/// `catalog_keys`).
pub const KEY_DISPLAY_SEPARATOR: char = '/';

/// Which way `:bump` walks from the cursor when an axis word is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BumpAxis {
    /// Every cell in the cursor's row — the default without a selection,
    /// because a term's whole node ladder is the shape a trader nudges.
    #[default]
    Row,
    Col,
}

/// A parsed `:` line.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Key parts split on [`KEY_DISPLAY_SEPARATOR`]. Dataset compatibility is
    /// checked by the document-query path rather than this parser.
    Key(Vec<String>),
    Revert,
    /// Add `delta` to numbers. With no axis: the selection when one is
    /// live, else the cursor's row. An axis word keeps its own meaning
    /// even with a selection live.
    Bump {
        delta: f64,
        axis: Option<BumpAxis>,
    },
    Rebase,
    /// Optional upload-target name. The tile resolves eligibility and requires
    /// an explicit target when more than one is available.
    Upload(Option<String>),
    /// Open the action list also reached through the menu key and header button.
    Menu,
    /// Set a document attribute without moving the cursor. None asks the tile
    /// to show its current value; type validation belongs to the tile.
    Set {
        attr: String,
        value: Option<String>,
    },
    /// Set the new-document policy, or ask the tile for its current value.
    Auto(Option<UpdatePolicy>),
    /// Fit every column to its content, or with `reset` return to the
    /// default widths.
    Autosize {
        reset: bool,
    },
}

/// Completion verbs in declared order. The parser also accepts `key` as an
/// alias, but never suggests it. Rebase is offered only when `behind` is true.
pub(crate) const VERBS: [&str; 9] = [
    "underlying",
    "revert",
    "bump",
    "rebase",
    "upload",
    "set",
    "auto",
    "menu",
    "autosize",
];

/// `auto`'s second word: the three policies, as [`UpdatePolicy::as_str`]
/// spells them, in [`UpdatePolicy::ALL`]'s order.
fn policy_words() -> Vec<String> {
    UpdatePolicy::ALL
        .iter()
        .map(|p| p.as_str().to_string())
        .collect()
}

fn behind_only(verb: &str) -> bool {
    verb == "rebase"
}

/// Parse a line without its leading colon. Names are case-sensitive.
/// Underlying and upload reject extra arguments, as does auto. Set joins its
/// value words with single spaces. Revert, rebase, and menu ignore trailing
/// words; bump reads a delta and optional axis without checking the remaining tail.
pub fn parse(line: &str) -> Result<Command, String> {
    let mut words = line.split_whitespace();
    match words.next() {
        Some(verb @ ("underlying" | "key")) => {
            let value = words
                .next()
                .ok_or_else(|| format!("usage: {verb} <value>"))?;
            if words.next().is_some() {
                return Err(format!(
                    "an underlying is one word; parts are separated by '{KEY_DISPLAY_SEPARATOR}'"
                ));
            }
            let parts: Vec<String> = value
                .split(KEY_DISPLAY_SEPARATOR)
                .map(str::to_string)
                .collect();
            if parts.iter().any(|p| p.is_empty()) {
                return Err(format!("'{value}' has an empty part"));
            }
            // A key part carrying the storage separator would join back
            // into a different key than the one typed — refused here
            // rather than silently reinterpreted downstream.
            if parts.iter().any(|p| p.contains(KEY_SEPARATOR)) {
                return Err("a key part may not contain the storage separator".to_string());
            }
            Ok(Command::Key(parts))
        }
        Some("revert") => Ok(Command::Revert),
        Some("bump") => {
            let delta = words
                .next()
                .ok_or_else(|| "usage: bump <delta> [row|col]".to_string())?;
            let delta: f64 = delta
                .parse()
                .map_err(|_| format!("'{delta}' is not a number"))?;
            if !delta.is_finite() {
                return Err(format!("'{delta}' is not a finite number"));
            }
            let axis = match words.next() {
                None => None,
                Some("row") => Some(BumpAxis::Row),
                Some("col") => Some(BumpAxis::Col),
                Some(other) => return Err(format!("unknown axis '{other}' (row, col)")),
            };
            Ok(Command::Bump { delta, axis })
        }
        Some("rebase") => Ok(Command::Rebase),
        Some("upload") => {
            let target = words.next().map(str::to_string);
            if words.next().is_some() {
                return Err("usage: upload [target]".to_string());
            }
            Ok(Command::Upload(target))
        }
        Some("set") => {
            let attr = words
                .next()
                .ok_or_else(|| "usage: set <attribute> [value]".to_string())?;
            // Preserve a multiword attribute value with normalized spacing. The tile
            // performs type validation after parsing the command.
            let tail: Vec<&str> = words.collect();
            let value = (!tail.is_empty()).then(|| tail.join(" "));
            Ok(Command::Set {
                attr: attr.to_string(),
                value,
            })
        }
        Some("auto") => {
            let policy = match words.next() {
                None => None,
                Some(word) => Some(UpdatePolicy::parse(word).ok_or_else(|| {
                    format!("unknown policy '{word}' ({})", policy_words().join(", "))
                })?),
            };
            if words.next().is_some() {
                return Err("usage: auto [hold|rebase|replace]".to_string());
            }
            Ok(Command::Auto(policy))
        }
        Some("menu") => Ok(Command::Menu),
        Some("autosize") => {
            let reset = match words.next() {
                None => false,
                Some("reset") => true,
                Some(_) => return Err("usage: autosize [reset]".to_string()),
            };
            if words.next().is_some() {
                return Err("usage: autosize [reset]".to_string());
            }
            Ok(Command::Autosize { reset })
        }
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Err("empty command".to_string()),
    }
}

/// Return unfiltered candidates for the cursor's token, not whole command lines.
/// The shell ranks them and replaces the token when a completion is accepted.
///
/// Keys use slash-separated display spelling; attrs and targets are supplied
/// by the tile. Rebase is offered only while behind. Cursor offsets are bytes;
/// commas and whitespace delimit completion tokens, unlike parse's whitespace-only
/// word splitting.
pub fn completions(
    line: &str,
    cursor: usize,
    keys: &[String],
    behind: bool,
    attrs: &[String],
    targets: &[String],
) -> Vec<String> {
    // Clamp a supplied byte cursor backward to a UTF-8 boundary before slicing.
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let head = &line[..cursor];
    // Split the way the shell's `commandline::word_at` splits, so the
    // position computed here is the word the shell will splice over.
    let mut words: Vec<&str> = head
        .split(|c: char| c.is_whitespace() || c == ',')
        .collect();
    // The word under the cursor is the shell's to rank; only the words
    // before it decide which position is being completed.
    words.pop();
    words.retain(|w| !w.is_empty());
    match words.as_slice() {
        [] => VERBS
            .iter()
            .filter(|v| behind || !behind_only(v))
            .map(|v| (*v).to_string())
            .collect(),
        ["underlying"] | ["key"] => keys.to_vec(),
        ["set"] => attrs.to_vec(),
        ["upload"] => targets.to_vec(),
        ["auto"] => policy_words(),
        ["autosize"] => vec!["reset".to_string()],
        // `bump`'s delta is a number nothing can complete; its axis is a
        // two-word vocabulary.
        ["bump", _] => vec!["row".to_string(), "col".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_splits_on_the_display_separator_and_refuses_a_blank_part() {
        assert_eq!(parse("key SPX.Z"), Ok(Command::Key(vec!["SPX.Z".into()])));
        assert_eq!(
            parse("key SPX.Z/EOD"),
            Ok(Command::Key(vec!["SPX.Z".into(), "EOD".into()])),
            "a multi-part key is typed with the display separator"
        );
        assert!(parse("key").is_err(), "no value");
        assert!(parse("key SPX.Z EOD").is_err(), "two words is not a key");
        assert!(parse("key SPX.Z/").is_err(), "an empty part");
        assert!(
            parse(&format!("key SPX{KEY_SEPARATOR}Z")).is_err(),
            "the storage separator is not typeable into a part"
        );
    }

    #[test]
    fn bump_reads_a_delta_and_an_optional_axis() {
        assert_eq!(
            parse("bump 0.25"),
            Ok(Command::Bump {
                delta: 0.25,
                axis: None
            }),
            "no word is no axis: the tile decides"
        );
        assert_eq!(
            parse("bump 0.25 row"),
            Ok(Command::Bump {
                delta: 0.25,
                axis: Some(BumpAxis::Row)
            })
        );
        assert_eq!(
            parse("bump 1 col"),
            Ok(Command::Bump {
                delta: 1.0,
                axis: Some(BumpAxis::Col)
            })
        );
        assert_eq!(
            parse("bump -1 col"),
            Ok(Command::Bump {
                delta: -1.0,
                axis: Some(BumpAxis::Col)
            })
        );
        assert!(parse("bump").is_err());
        assert!(parse("bump wide").is_err());
        assert!(parse("bump inf").is_err(), "not a finite number");
        assert!(parse("bump 1 diagonal").is_err());
    }

    #[test]
    fn the_other_verbs_parse_and_an_unknown_one_is_named() {
        assert_eq!(parse("revert"), Ok(Command::Revert));
        assert_eq!(parse("rebase"), Ok(Command::Rebase));
        assert_eq!(parse("rebse"), Err("unknown command 'rebse'".to_string()));
        assert_eq!(parse("   "), Err("empty command".to_string()));
    }

    #[test]
    fn autosize_parses_an_optional_reset_and_completes_it() {
        assert_eq!(parse("autosize"), Ok(Command::Autosize { reset: false }));
        assert_eq!(
            parse("autosize reset"),
            Ok(Command::Autosize { reset: true })
        );
        assert!(parse("autosize wide").is_err());
        assert!(parse("autosize reset now").is_err());
        assert_eq!(
            completions("autosize ", 9, &[], false, &[], &[]),
            vec!["reset".to_string()]
        );
    }

    #[test]
    fn completions_offer_the_verbs_then_the_catalog_keys() {
        let keys = vec!["NDX.Z".to_string(), "SPX.Z".to_string()];
        assert_eq!(
            completions("", 0, &keys, false, &[], &[]),
            vec![
                "underlying",
                "revert",
                "bump",
                "upload",
                "set",
                "auto",
                "menu",
                "autosize"
            ],
            "rebase is offered only while behind"
        );
        assert_eq!(
            completions("", 0, &keys, true, &[], &[]),
            vec![
                "underlying",
                "revert",
                "bump",
                "rebase",
                "upload",
                "set",
                "auto",
                "menu",
                "autosize"
            ]
        );
        assert_eq!(completions("key ", 4, &keys, false, &[], &[]), keys);
        assert_eq!(
            completions("underlying ", 11, &keys, false, &[], &[]),
            keys,
            "both underlying and key (the alias) complete with catalog keys"
        );
        assert_eq!(
            completions("underlying SP", 13, &keys, false, &[], &[]),
            keys,
            "the whole vocabulary, unfiltered — the shell ranks it"
        );
        assert_eq!(
            completions("bump 1 ", 7, &keys, false, &[], &[]),
            vec!["row", "col"]
        );
        assert_eq!(
            completions("bump ", 5, &keys, false, &[], &[]),
            Vec::<String>::new(),
            "nothing completes a number"
        );
        assert_eq!(
            completions("revert ", 7, &keys, false, &[], &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cursor_off_a_char_boundary_does_not_panic() {
        let keys = vec!["SPX.Z".to_string()];
        // `é` is two bytes: a cursor at 2 lands mid-char.
        assert!(!completions("kéy", 2, &keys, false, &[], &[]).is_empty());
    }

    #[test]
    fn underlying_parses_like_key_and_key_stays_an_alias() {
        assert_eq!(
            parse("underlying SPX.Z"),
            Ok(Command::Key(vec!["SPX.Z".into()]))
        );
        assert_eq!(parse("key SPX.Z"), Ok(Command::Key(vec!["SPX.Z".into()])));
        assert_eq!(parse("underlying"), Err("usage: underlying <value>".into()));
    }

    #[test]
    fn completions_offer_underlying_and_never_the_key_alias() {
        let verbs = completions("", 0, &[], false, &[], &[]);
        assert_eq!(verbs[0], "underlying");
        assert!(!verbs.iter().any(|v| v == "key"), "{verbs:?}");
    }

    #[test]
    fn set_parses_an_attribute_with_or_without_a_value() {
        assert_eq!(
            parse("set spot_ref 4520"),
            Ok(Command::Set {
                attr: "spot_ref".into(),
                value: Some("4520".into())
            })
        );
        assert_eq!(
            parse("set spot_ref"),
            Ok(Command::Set {
                attr: "spot_ref".into(),
                value: None
            })
        );
        assert_eq!(parse("set"), Err("usage: set <attribute> [value]".into()));
        // A multi-word value is joined with single spaces, whatever the
        // trader's own spacing was.
        assert_eq!(
            parse("set note  front   month"),
            Ok(Command::Set {
                attr: "note".into(),
                value: Some("front month".into())
            })
        );
    }

    #[test]
    fn auto_parses_a_policy_and_a_bare_auto_asks_the_tile() {
        assert_eq!(
            parse("auto rebase"),
            Ok(Command::Auto(Some(UpdatePolicy::Rebase)))
        );
        assert_eq!(
            parse("auto hold"),
            Ok(Command::Auto(Some(UpdatePolicy::Hold)))
        );
        assert_eq!(
            parse("auto replace"),
            Ok(Command::Auto(Some(UpdatePolicy::Replace)))
        );
        // A bare `auto` is a question the tile answers with the current
        // policy — parsed, not refused, exactly as `set <attr>` is.
        assert_eq!(parse("auto"), Ok(Command::Auto(None)));
        assert_eq!(
            parse("auto discard"),
            Err("unknown policy 'discard' (hold, rebase, replace)".into())
        );
        assert_eq!(
            parse("auto rebase now"),
            Err("usage: auto [hold|rebase|replace]".into())
        );
    }

    #[test]
    fn auto_completes_the_three_policies() {
        assert_eq!(
            completions("auto ", 5, &[], false, &[], &[]),
            vec!["hold", "rebase", "replace"]
        );
        assert_eq!(
            completions("auto re", 7, &[], false, &[], &[]),
            vec!["hold", "rebase", "replace"],
            "the whole vocabulary — the shell ranks it"
        );
    }

    #[test]
    fn set_completes_attribute_names() {
        let c = completions(
            "set ",
            4,
            &[],
            false,
            &["anchor_date".into(), "spot_ref".into()],
            &[],
        );
        assert_eq!(c, vec!["anchor_date", "spot_ref"]);
    }

    #[test]
    fn upload_parses_with_and_without_a_target() {
        assert_eq!(parse("upload"), Ok(Command::Upload(None)));
        assert_eq!(
            parse("upload sophis"),
            Ok(Command::Upload(Some("sophis".into())))
        );
        assert_eq!(
            parse("upload sophis bbg"),
            Err("usage: upload [target]".into()),
            "one target per upload"
        );
    }

    #[test]
    fn upload_completes_the_eligible_targets() {
        let targets = vec!["sophis".to_string(), "bbg".to_string()];
        assert_eq!(
            completions("upload ", 7, &[], false, &[], &targets),
            targets,
            "the eligible targets, in egress order"
        );
        assert!(
            completions("", 0, &[], false, &[], &targets)
                .iter()
                .any(|v| v == "upload"),
            "upload is offered as a verb"
        );
    }
}
