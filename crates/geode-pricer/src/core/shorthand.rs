//! The one-line shorthand (line-pricer spec §6.3), parsed here and
//! rendered here (Task 3), so the two halves share one table of tokens.
//!
//! `[qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]`, whitespace-
//! separated, case-insensitive. A month form (`Z26`, `DEC26`) resolves to
//! the third Friday of its month at parse time: a date CONVENTION, not a
//! financial calculation (the spec says so); holidays are not considered.
//! A tenor (`3m`) is validated by `Expiry::tenor` and never resolved —
//! that is the library's calendar.

use crate::core::template::Template;
use chrono::{Datelike, NaiveDate, Weekday};
use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, OptionKind, Strike, Vanilla};

/// A line's own shifts; `None` inherits the sheet's (spec ruling 8).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnShifts {
    pub spot_pct: Option<f64>,
    pub vol_pts: Option<f64>,
}

/// One line as the parser or a caller describes it, before it has an id.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpec {
    pub instrument: Instrument,
    /// Signed; a sell is negative; never zero.
    pub qty: i64,
    pub shift: OwnShifts,
}

/// What one shorthand line means: a line, or a package with its legs
/// (planning decision 1).
#[derive(Debug, Clone, PartialEq)]
pub enum RowSpec {
    Line(LineSpec),
    Package {
        template: Template,
        legs: Vec<LineSpec>,
    },
}

/// `offset` is the byte offset of the offending token in the text the
/// caller passed (`text.len()` when a token is missing), for the footer's
/// caret (spec §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

/// IMM month codes, January to December (spec §6.3). One constant, one test.
pub const IMM_MONTHS: [char; 12] = ['F', 'G', 'H', 'J', 'K', 'M', 'N', 'Q', 'U', 'V', 'X', 'Z'];
pub const MONTH_NAMES: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

/// The third Friday of `month` in `year`; `None` for a month outside 1–12.
pub fn third_friday(year: i32, month: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let to_friday =
        (Weekday::Fri.num_days_from_monday() + 7 - first.weekday().num_days_from_monday()) % 7;
    first.checked_add_days(chrono::Days::new(u64::from(to_friday) + 14))
}

fn month_index(name: &str) -> Option<u32> {
    MONTH_NAMES
        .iter()
        .position(|m| *m == name)
        .map(|i| i as u32 + 1)
}

/// A two-digit year is `20yy` (spec §6.3).
fn year_of(two: &str) -> Option<i32> {
    if two.len() != 2 || !two.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    two.parse::<i32>().ok().map(|y| 2000 + y)
}

/// `Z26` | `DEC26` | `20DEC26` | `3m` — the month forms resolve to the
/// third Friday; a full date is itself; anything else must be a tenor.
pub fn parse_expiry(token: &str) -> Result<Expiry, String> {
    let upper = token.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    // IMM code: one letter, two digits.
    if bytes.len() == 3 && bytes[0].is_ascii_alphabetic() {
        if let Some(m) = IMM_MONTHS.iter().position(|c| *c as u8 == bytes[0])
            && let Some(y) = year_of(&upper[1..])
        {
            return third_friday(y, m as u32 + 1)
                .map(Expiry::Date)
                .ok_or_else(|| format!("expiry '{token}': no third Friday"));
        }
        return Err(format!(
            "expiry '{token}': not a month code, a month name, a date or a tenor"
        ));
    }
    // Month name: three letters, two digits.
    if bytes.len() == 5 && bytes[..3].iter().all(u8::is_ascii_alphabetic) {
        let month = month_index(&upper[..3])
            .ok_or_else(|| format!("expiry '{token}': '{}' is not a month", &upper[..3]))?;
        let year = year_of(&upper[3..])
            .ok_or_else(|| format!("expiry '{token}': expected a two-digit year"))?;
        return third_friday(year, month)
            .map(Expiry::Date)
            .ok_or_else(|| format!("expiry '{token}': no third Friday"));
    }
    // Full date: one or two digits, three letters, two digits.
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=2).contains(&digits)
        && bytes.len() == digits + 5
        && bytes[digits..digits + 3]
            .iter()
            .all(u8::is_ascii_alphabetic)
    {
        let day: u32 = upper[..digits]
            .parse()
            .map_err(|_| format!("expiry '{token}': bad day"))?;
        let month = month_index(&upper[digits..digits + 3]).ok_or_else(|| {
            format!(
                "expiry '{token}': '{}' is not a month",
                &upper[digits..digits + 3]
            )
        })?;
        let year = year_of(&upper[digits + 3..])
            .ok_or_else(|| format!("expiry '{token}': expected a two-digit year"))?;
        return NaiveDate::from_ymd_opt(year, month, day)
            .map(Expiry::Date)
            .ok_or_else(|| format!("expiry '{token}': no such day"));
    }
    Expiry::tenor(token)
}

/// `5000` | `4250.5` | `95%` — positive numbers only.
pub fn parse_strike(token: &str) -> Result<Strike, String> {
    let (number, percent) = match token.strip_suffix('%') {
        Some(n) => (n, true),
        None => (token, false),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("strike '{token}': not a number"))?;
    if !(value.is_finite() && value > 0.0) {
        return Err(format!("strike '{token}': must be a positive number"));
    }
    Ok(if percent {
        Strike::Percent(value)
    } else {
        Strike::Absolute(value)
    })
}

fn parse_barrier_kind(token: &str) -> Option<BarrierKind> {
    match token.to_ascii_uppercase().as_str() {
        "UI" => Some(BarrierKind::UpIn),
        "UO" => Some(BarrierKind::UpOut),
        "DI" => Some(BarrierKind::DownIn),
        "DO" => Some(BarrierKind::DownOut),
        _ => None,
    }
}

/// A token and where it starts.
struct Tok<'a> {
    text: &'a str,
    offset: usize,
}

fn tokens(text: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push(Tok {
                    text: &text[s..i],
                    offset: s,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(Tok {
            text: &text[s..],
            offset: s,
        });
    }
    out
}

fn err(offset: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        offset,
        message: message.into(),
    }
}

/// One line of shorthand to a line or a package (spec §6.3).
pub fn parse(text: &str) -> Result<RowSpec, ParseError> {
    let toks = tokens(text);
    let end = text.len();
    if toks.is_empty() {
        return Err(err(
            0,
            "empty line: [qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]",
        ));
    }
    let mut i = 0;
    // qty: a signed integer, else 1 and the token is the underlying.
    let qty = match toks[0].text.parse::<i64>() {
        Ok(0) => return Err(err(toks[0].offset, "quantity must not be zero")),
        Ok(q) => {
            i += 1;
            q
        }
        Err(_) => 1,
    };
    /// The token at `i`, or "expected <what>" pointing past the end.
    fn next<'t>(
        toks: &'t [Tok<'t>],
        i: usize,
        end: usize,
        what: &str,
    ) -> Result<&'t Tok<'t>, ParseError> {
        toks.get(i)
            .ok_or_else(|| err(end, format!("expected {what}")))
    }
    let underlying = next(&toks, i, end, "an underlying")?
        .text
        .to_ascii_uppercase();
    i += 1;
    let expiry_tok = next(&toks, i, end, "an expiry")?;
    i += 1;
    let expiries: Vec<Expiry> = expiry_tok
        .text
        .split('/')
        .map(|t| parse_expiry(t).map_err(|m| err(expiry_tok.offset, m)))
        .collect::<Result<_, _>>()?;
    let strikes_tok = next(&toks, i, end, "one or more strikes")?;
    i += 1;
    let strikes: Vec<Strike> = strikes_tok
        .text
        .split('/')
        .map(|t| parse_strike(t).map_err(|m| err(strikes_tok.offset, m)))
        .collect::<Result<_, _>>()?;
    let type_tok = next(&toks, i, end, "a type: C P CS PS STRD STRG RR FLY CAL")?;
    i += 1;
    let type_upper = type_tok.text.to_ascii_uppercase();

    let count = |n: usize, noun: &str| -> String {
        if n == 1 {
            format!("1 {noun}")
        } else {
            let plural = if noun == "expiry" {
                "expiries".to_string()
            } else {
                format!("{noun}s")
            };
            format!("{n} {plural}")
        }
    };

    let single = match type_upper.as_str() {
        "C" => Some(OptionKind::Call),
        "P" => Some(OptionKind::Put),
        _ => None,
    };
    if let Some(kind) = single {
        if strikes.len() != 1 {
            return Err(err(
                strikes_tok.offset,
                format!("{type_upper} takes 1 strike, got {}", strikes.len()),
            ));
        }
        if expiries.len() != 1 {
            return Err(err(
                expiry_tok.offset,
                format!("{type_upper} takes 1 expiry, got {}", expiries.len()),
            ));
        }
        let vanilla = Vanilla {
            underlying,
            expiry: expiries.into_iter().next().expect("one expiry"),
            strike: strikes[0],
            kind,
        };
        let instrument = match toks.get(i) {
            None => Instrument::Vanilla(vanilla),
            Some(bk) => match parse_barrier_kind(bk.text) {
                Some(barrier) => {
                    i += 1;
                    let level_tok = next(&toks, i, end, "a barrier level")?;
                    i += 1;
                    let level: f64 = level_tok
                        .text
                        .parse()
                        .ok()
                        .filter(|l: &f64| l.is_finite() && *l > 0.0)
                        .ok_or_else(|| {
                            err(
                                level_tok.offset,
                                format!(
                                    "barrier level '{}': not a positive number",
                                    level_tok.text
                                ),
                            )
                        })?;
                    Instrument::Barrier(Barrier {
                        vanilla,
                        level,
                        barrier,
                    })
                }
                // A trailing token that isn't a barrier keyword: if
                // another token follows it, it reads as a mistyped
                // `BARRIER level` pair and the error names the barrier;
                // alone, it is just unexpected trailing text.
                None if toks.get(i + 1).is_some() => {
                    return Err(err(
                        bk.offset,
                        format!("'{}' is not a barrier kind (UI UO DI DO)", bk.text),
                    ));
                }
                None => {
                    return Err(err(bk.offset, format!("unexpected token '{}'", bk.text)));
                }
            },
        };
        if let Some(extra) = toks.get(i) {
            return Err(err(
                extra.offset,
                format!("unexpected token '{}'", extra.text),
            ));
        }
        return Ok(RowSpec::Line(LineSpec {
            instrument,
            qty,
            shift: OwnShifts::default(),
        }));
    }

    let template = Template::parse(&type_upper)
        .filter(|t| *t != Template::Custom)
        .ok_or_else(|| {
            err(
                type_tok.offset,
                format!(
                    "unknown type '{}': C P CS PS STRD STRG RR FLY CAL",
                    type_tok.text
                ),
            )
        })?;
    // Checked in token order (expiry precedes strikes in the grammar):
    // `SPX DEC26/MAR27 5000 CS` has both a wrong expiry count and a wrong
    // strike count, and the trader's cursor is still on the expiry token.
    if expiries.len() != template.expiries() {
        return Err(err(
            expiry_tok.offset,
            format!(
                "{} takes {}, got {}",
                template.token(),
                count(template.expiries(), "expiry"),
                expiries.len()
            ),
        ));
    }
    if strikes.len() != template.strikes() {
        return Err(err(
            strikes_tok.offset,
            format!(
                "{} takes {}, got {}",
                template.token(),
                count(template.strikes(), "strike"),
                strikes.len()
            ),
        ));
    }
    if let Some(extra) = toks.get(i) {
        let message = if parse_barrier_kind(extra.text).is_some() {
            "a barrier belongs on a single C or P leg, not on a package".to_string()
        } else {
            format!("unexpected token '{}'", extra.text)
        };
        return Err(err(extra.offset, message));
    }
    let legs = template
        .legs()
        .iter()
        .map(|l| LineSpec {
            instrument: Instrument::Vanilla(Vanilla {
                underlying: underlying.clone(),
                expiry: expiries[l.expiry].clone(),
                strike: strikes[l.strike],
                kind: l.kind,
            }),
            qty: qty * l.weight,
            shift: OwnShifts::default(),
        })
        .collect();
    Ok(RowSpec::Package { template, legs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::pricing::{BarrierKind, OptionKind, Strike};

    fn d(y: i32, m: u32, day: u32) -> Expiry {
        Expiry::Date(NaiveDate::from_ymd_opt(y, m, day).unwrap())
    }

    fn line(text: &str) -> LineSpec {
        match parse(text).unwrap() {
            RowSpec::Line(l) => l,
            other => panic!("{text:?} parsed as a package: {other:?}"),
        }
    }

    fn package(text: &str) -> (Template, Vec<LineSpec>) {
        match parse(text).unwrap() {
            RowSpec::Package { template, legs } => (template, legs),
            other => panic!("{text:?} parsed as a line: {other:?}"),
        }
    }

    #[test]
    fn a_month_code_resolves_to_the_third_friday() {
        // December 2026: the 1st is a Tuesday, the first Friday the 4th,
        // the third the 18th.
        assert_eq!(
            third_friday(2026, 12),
            Some(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap())
        );
        // A month starting on a Friday: the 1st IS the first Friday.
        // May 2026 starts on a Friday → third Friday the 15th.
        assert_eq!(
            third_friday(2026, 5),
            Some(NaiveDate::from_ymd_opt(2026, 5, 15).unwrap())
        );
        // A month starting on a Saturday: the first Friday is the 7th.
        // August 2026 starts on a Saturday → the 21st.
        assert_eq!(
            third_friday(2026, 8),
            Some(NaiveDate::from_ymd_opt(2026, 8, 21).unwrap())
        );
        assert_eq!(third_friday(2026, 13), None);
        assert_eq!(parse_expiry("Z26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("z26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("DEC26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("dec26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("K26").unwrap(), d(2026, 5, 15));
    }

    #[test]
    fn the_imm_table_is_the_twelve_letters_in_month_order() {
        assert_eq!(IMM_MONTHS.iter().collect::<String>(), "FGHJKMNQUVXZ");
        assert_eq!(MONTH_NAMES[0], "JAN");
        assert_eq!(MONTH_NAMES[11], "DEC");
        for (i, c) in IMM_MONTHS.iter().enumerate() {
            assert_eq!(
                parse_expiry(&format!("{c}27")).unwrap(),
                Expiry::Date(third_friday(2027, i as u32 + 1).unwrap())
            );
        }
    }

    #[test]
    fn every_expiry_form_parses() {
        assert_eq!(parse_expiry("20DEC26").unwrap(), d(2026, 12, 20));
        assert_eq!(parse_expiry("5dec26").unwrap(), d(2026, 12, 5));
        assert_eq!(parse_expiry("05DEC26").unwrap(), d(2026, 12, 5));
        assert_eq!(parse_expiry("3m").unwrap(), Expiry::Tenor("3m".into()));
        assert_eq!(parse_expiry("6W").unwrap(), Expiry::Tenor("6w".into()));
        assert_eq!(parse_expiry("1y").unwrap(), Expiry::Tenor("1y".into()));
        assert!(parse_expiry("31FEB26").is_err(), "no such day");
        assert!(parse_expiry("XYZ26").is_err(), "not a month");
        assert!(parse_expiry("DEC").is_err(), "no year");
        assert!(parse_expiry("3q").is_err(), "not a tenor unit");
        assert!(parse_expiry("").is_err());
    }

    #[test]
    fn a_strike_is_absolute_or_percent() {
        assert_eq!(parse_strike("5000").unwrap(), Strike::Absolute(5000.0));
        assert_eq!(parse_strike("4250.5").unwrap(), Strike::Absolute(4250.5));
        assert_eq!(parse_strike("95%").unwrap(), Strike::Percent(95.0));
        assert_eq!(parse_strike("102.5%").unwrap(), Strike::Percent(102.5));
        assert!(parse_strike("abc").is_err());
        assert!(parse_strike("%").is_err());
        assert!(parse_strike("").is_err());
        assert!(parse_strike("-5").is_err(), "a strike is positive");
    }

    #[test]
    fn a_vanilla_line_parses_with_and_without_a_quantity() {
        let l = line("SPX DEC26 5000 C");
        assert_eq!(l.qty, 1);
        assert_eq!(l.shift, OwnShifts::default());
        assert_eq!(
            l.instrument,
            Instrument::Vanilla(Vanilla {
                underlying: "SPX".into(),
                expiry: d(2026, 12, 18),
                strike: Strike::Absolute(5000.0),
                kind: OptionKind::Call,
            })
        );
        let l = line("-5 spx z26 95% p");
        assert_eq!(l.qty, -5);
        assert_eq!(l.instrument.underlying(), "SPX");
        assert_eq!(l.instrument.kind(), OptionKind::Put);
        assert_eq!(l.instrument.strike(), Strike::Percent(95.0));
        let l = line("10 NDX 3m 100% C");
        assert_eq!(l.qty, 10);
        assert_eq!(l.instrument.expiry(), &Expiry::Tenor("3m".into()));
    }

    #[test]
    fn a_barrier_line_parses_only_after_c_or_p() {
        let l = line("SPX DEC26 5000 C DO 4200");
        match l.instrument {
            Instrument::Barrier(b) => {
                assert_eq!(b.level, 4200.0);
                assert_eq!(b.barrier, BarrierKind::DownOut);
                assert_eq!(b.vanilla.kind, OptionKind::Call);
            }
            other => panic!("{other:?}"),
        }
        for (token, kind) in [
            ("UI", BarrierKind::UpIn),
            ("uo", BarrierKind::UpOut),
            ("DI", BarrierKind::DownIn),
            ("do", BarrierKind::DownOut),
        ] {
            let l = line(&format!("SPX DEC26 5000 P {token} 4000"));
            match l.instrument {
                Instrument::Barrier(b) => assert_eq!(b.barrier, kind),
                other => panic!("{other:?}"),
            }
        }
        let e = parse("SPX DEC26 95%/105% CS DO 4200").unwrap_err();
        assert_eq!(e.offset, 22, "the barrier token: {e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse("SPX DEC26 5000 C DO").unwrap_err();
        assert_eq!(e.offset, 19, "a missing level points past the end: {e:?}");
        let e = parse("SPX DEC26 5000 C XX 4200").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse("SPX DEC26 5000 C DO abc").unwrap_err();
        assert_eq!(e.offset, 20, "{e:?}");
    }

    #[test]
    fn every_template_expands_over_its_strikes_and_expiries() {
        let (t, legs) = package("-5 SPX DEC26 95%/105% CS");
        assert_eq!(t, Template::CS);
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].qty, -5);
        assert_eq!(legs[1].qty, 5);
        assert_eq!(legs[0].instrument.strike(), Strike::Percent(95.0));
        assert_eq!(legs[1].instrument.strike(), Strike::Percent(105.0));
        assert!(legs.iter().all(|l| l.instrument.kind() == OptionKind::Call));
        assert!(legs.iter().all(|l| l.instrument.underlying() == "SPX"));

        let (t, legs) = package("SPX DEC26 4800/5200 PS");
        assert_eq!(t, Template::PS);
        assert_eq!((legs[0].qty, legs[1].qty), (1, -1));
        assert!(legs.iter().all(|l| l.instrument.kind() == OptionKind::Put));

        let (t, legs) = package("2 SPX DEC26 5000 STRD");
        assert_eq!(t, Template::STRD);
        assert_eq!((legs[0].qty, legs[1].qty), (2, 2));
        assert_eq!(
            (legs[0].instrument.kind(), legs[1].instrument.kind()),
            (OptionKind::Call, OptionKind::Put)
        );

        let (t, legs) = package("SPX DEC26 4800/5200 STRG");
        assert_eq!(t, Template::STRG);
        assert_eq!(legs[0].instrument.kind(), OptionKind::Put);
        assert_eq!(legs[0].instrument.strike(), Strike::Absolute(4800.0));
        assert_eq!(legs[1].instrument.kind(), OptionKind::Call);
        assert_eq!(legs[1].instrument.strike(), Strike::Absolute(5200.0));

        let (t, legs) = package("SPX DEC26 4800/5200 RR");
        assert_eq!(t, Template::RR);
        assert_eq!((legs[0].qty, legs[1].qty), (-1, 1));
        assert_eq!(legs[0].instrument.kind(), OptionKind::Put);

        let (t, legs) = package("3 SPX DEC26 4800/5000/5200 FLY");
        assert_eq!(t, Template::FLY);
        assert_eq!(
            legs.iter().map(|l| l.qty).collect::<Vec<_>>(),
            vec![3, -6, 3]
        );

        let (t, legs) = package("SPX DEC26/MAR27 5000 CAL");
        assert_eq!(t, Template::CAL);
        assert_eq!(legs[0].qty, 1);
        assert_eq!(legs[0].instrument.expiry(), &d(2027, 3, 19), "+far first");
        assert_eq!(legs[1].qty, -1);
        assert_eq!(
            legs[1].instrument.expiry(),
            &d(2026, 12, 18),
            "−near second"
        );
    }

    #[test]
    fn every_error_names_the_offending_offset() {
        let e = parse("").unwrap_err();
        assert_eq!(e.offset, 0);
        assert!(e.message.contains("empty"), "{e:?}");
        let e = parse("   ").unwrap_err();
        assert!(e.message.contains("empty"), "{e:?}");

        let e = parse("0 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 0, "{e:?}");
        assert!(e.message.contains("zero"), "{e:?}");

        let e = parse("SPX").unwrap_err();
        assert_eq!(e.offset, 3, "missing expiry points past the end: {e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse("SPX DEX26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse("SPX DEC26 abc C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("strike"), "{e:?}");

        let e = parse("SPX DEC26 5000").unwrap_err();
        assert_eq!(e.offset, 14, "missing type: {e:?}");
        assert!(e.message.contains("type"), "{e:?}");

        let e = parse("SPX DEC26 5000 XYZ").unwrap_err();
        assert_eq!(e.offset, 15, "{e:?}");
        assert!(e.message.contains("unknown type"), "{e:?}");

        let e = parse("SPX DEC26 95%/105%/110% CS").unwrap_err();
        assert_eq!(e.offset, 10, "the strikes token: {e:?}");
        assert!(e.message.contains("CS takes 2 strikes"), "{e:?}");

        let e = parse("SPX DEC26 5000/5200 C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("1 strike"), "{e:?}");

        let e = parse("SPX DEC26/MAR27 5000 CS").unwrap_err();
        assert_eq!(e.offset, 4, "the expiries token: {e:?}");
        assert!(e.message.contains("CS takes 1 expiry"), "{e:?}");

        let e = parse("SPX DEC26 5000 CAL").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("CAL takes 2 expiries"), "{e:?}");

        let e = parse("SPX DEC26 5000 C extra").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("unexpected"), "{e:?}");
    }

    #[test]
    fn quantities_that_are_not_integers_are_the_underlying() {
        // "1.5" is not an integer, so it is read as an underlying named
        // "1.5" — the grammar has no fractional quantity, and the trader
        // sees the error at the next token.
        let e = parse("1.5 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");
        // "+3" is a quantity.
        assert_eq!(line("+3 SPX DEC26 5000 C").qty, 3);
    }
}
