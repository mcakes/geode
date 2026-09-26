//! The one-line shorthand (line-pricer spec §6.3), parsed here and
//! rendered here, so the two halves share one table of tokens.
//!
//! `[qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]`, whitespace-
//! separated, case-insensitive. A month form (`Z26`, `DEC26`) resolves to
//! the third Friday of its month at parse time: a date CONVENTION, not a
//! financial calculation (the spec says so); holidays are not considered.
//! A tenor (`3m`) is validated by `Expiry::tenor` and never resolved —
//! that is the library's calendar.

use crate::core::sheet::{LineSpec, OwnShifts, RowSpec};
use crate::core::template::{Template, TemplateDef, TemplateSet};
use chrono::{Datelike, NaiveDate, Weekday};
use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, OptionKind, Strike, Vanilla};

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

pub(crate) fn parse_barrier_kind(token: &str) -> Option<BarrierKind> {
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

/// One line of shorthand to a line or a package (spec §6.3). A package's
/// type token must name a table in `templates`; `C` and `P` need none.
pub fn parse(text: &str, templates: &TemplateSet) -> Result<RowSpec, ParseError> {
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

    let def = templates.resolve(&type_upper).ok_or_else(|| {
        // `C`, `P`, then the set's names, single-spaced: an empty set
        // leaves no trailing space.
        let names: Vec<&str> = ["C", "P"]
            .into_iter()
            .chain(templates.iter().map(|d| d.name.as_str()))
            .collect();
        err(
            type_tok.offset,
            format!("unknown type '{}': {}", type_tok.text, names.join(" ")),
        )
    })?;
    let template = Template::named(&def.name);
    // Checked in token order (expiry precedes strikes in the grammar):
    // `SPX DEC26/MAR27 5000 CS` has both a wrong expiry count and a wrong
    // strike count, and the trader's cursor is still on the expiry token.
    if expiries.len() != def.expiries {
        return Err(err(
            expiry_tok.offset,
            format!(
                "{} takes {}, got {}",
                def.name,
                count(def.expiries, "expiry"),
                expiries.len()
            ),
        ));
    }
    if strikes.len() != def.strikes {
        return Err(err(
            strikes_tok.offset,
            format!(
                "{} takes {}, got {}",
                def.name,
                count(def.strikes, "strike"),
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
    // A template weight multiplies the typed quantity: `i64::MAX FLY`
    // would panic in debug and wrap in release, so it is a parse error
    // named at the quantity token (which is `toks[0]` whenever one was
    // read; with no quantity the weight multiplies 1 and cannot overflow).
    let qty_offset = toks[0].offset;
    let legs = def
        .legs
        .iter()
        .map(|l| {
            Ok(LineSpec {
                instrument: Instrument::Vanilla(Vanilla {
                    underlying: underlying.clone(),
                    expiry: expiries[l.expiry].clone(),
                    strike: strikes[l.strike],
                    kind: l.kind,
                }),
                qty: qty
                    .checked_mul(l.weight)
                    .ok_or_else(|| err(qty_offset, "quantity out of range"))?,
                shift: OwnShifts::default(),
            })
        })
        .collect::<Result<Vec<LineSpec>, ParseError>>()?;
    Ok(RowSpec::Package { template, legs })
}

/// `Z26` when `date` is the third Friday of its month, else `None`.
pub fn imm_code(date: NaiveDate) -> Option<String> {
    if third_friday(date.year(), date.month()) != Some(date) {
        return None;
    }
    let letter = IMM_MONTHS[date.month0() as usize];
    Some(format!("{letter}{:02}", date.year() % 100))
}

/// A third Friday as its IMM code, any other date as `20DEC26`, a tenor
/// as stored (spec §6.3).
pub fn render_expiry(expiry: &Expiry) -> String {
    match expiry {
        Expiry::Date(d) => imm_code(*d).unwrap_or_else(|| {
            format!(
                "{:02}{}{:02}",
                d.day(),
                MONTH_NAMES[d.month0() as usize],
                d.year() % 100
            )
        }),
        Expiry::Tenor(t) => t.clone(),
    }
}

/// `f64`'s own `Display` prints `5000.0` as `5000` and `4250.5` as
/// `4250.5`: the shortest text that parses back to the same number.
pub fn render_strike(strike: Strike) -> String {
    match strike {
        Strike::Absolute(k) => format!("{k}"),
        Strike::Percent(p) => format!("{p}%"),
    }
}

pub fn render_barrier_kind(kind: BarrierKind) -> &'static str {
    match kind {
        BarrierKind::UpIn => "UI",
        BarrierKind::UpOut => "UO",
        BarrierKind::DownIn => "DI",
        BarrierKind::DownOut => "DO",
    }
}

fn kind_token(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::Call => "C",
        OptionKind::Put => "P",
    }
}

fn qty_prefix(qty: i64) -> String {
    if qty == 1 {
        String::new()
    } else {
        format!("{qty} ")
    }
}

/// One line back in the grammar. A qty of 1 is omitted, as the grammar
/// defaults it.
pub fn render_line(qty: i64, instrument: &Instrument) -> String {
    let v = instrument.vanilla();
    let mut out = format!(
        "{}{} {} {} {}",
        qty_prefix(qty),
        v.underlying,
        render_expiry(&v.expiry),
        render_strike(v.strike),
        kind_token(v.kind)
    );
    if let Instrument::Barrier(b) = instrument {
        out.push_str(&format!(" {} {}", render_barrier_kind(b.barrier), b.level));
    }
    out
}

/// The template form (`-5 SPX Z26 95%/105% CS`) when `legs` still match
/// `def`'s table: same count, every leg a vanilla on one
/// underlying, each leg's qty the package qty times its weight, each
/// leg's kind the table's, and one strike per strike index and one
/// expiry per expiry index across the legs. `None` otherwise — the
/// caller prints the legs one per line (planning decision 11).
pub fn render_package(def: &TemplateDef, legs: &[(i64, &Instrument)]) -> Option<String> {
    let table = &def.legs;
    if table.is_empty() || table.len() != legs.len() {
        return None;
    }
    let first = table[0];
    let (q0, _) = legs[0];
    if q0 % first.weight != 0 {
        return None;
    }
    let qty = q0 / first.weight;
    if qty == 0 {
        return None;
    }
    let underlying = legs[0].1.underlying();
    let mut strikes: Vec<Option<Strike>> = vec![None; def.strikes];
    let mut expiries: Vec<Option<&Expiry>> = vec![None; def.expiries];
    for (spec, (leg_qty, instrument)) in table.iter().zip(legs) {
        let Instrument::Vanilla(v) = instrument else {
            return None;
        };
        if *leg_qty != qty * spec.weight || v.kind != spec.kind || v.underlying != underlying {
            return None;
        }
        match strikes[spec.strike] {
            None => strikes[spec.strike] = Some(v.strike),
            Some(k) if k == v.strike => {}
            Some(_) => return None,
        }
        match expiries[spec.expiry] {
            None => expiries[spec.expiry] = Some(&v.expiry),
            Some(e) if *e == v.expiry => {}
            Some(_) => return None,
        }
    }
    let strikes: Vec<String> = strikes
        .into_iter()
        .map(|k| k.map(render_strike))
        .collect::<Option<_>>()?;
    let expiries: Vec<String> = expiries
        .into_iter()
        .map(|e| e.map(render_expiry))
        .collect::<Option<_>>()?;
    Some(format!(
        "{}{} {} {} {}",
        qty_prefix(qty),
        underlying,
        expiries.join("/"),
        strikes.join("/"),
        def.name
    ))
}

/// `parse` against the built-in tables, for tests. `TemplateSet::builtin`
/// parses TOML, so each test thread builds it once.
#[cfg(test)]
pub(crate) fn parse_builtin(text: &str) -> Result<RowSpec, ParseError> {
    thread_local! {
        static BUILTIN: TemplateSet = TemplateSet::builtin();
    }
    BUILTIN.with(|set| parse(text, set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::pricing::{BarrierKind, OptionKind, Strike};

    fn builtin() -> TemplateSet {
        TemplateSet::builtin()
    }

    fn d(y: i32, m: u32, day: u32) -> Expiry {
        Expiry::Date(NaiveDate::from_ymd_opt(y, m, day).unwrap())
    }

    fn line(text: &str) -> LineSpec {
        match parse_builtin(text).unwrap() {
            RowSpec::Line(l) => l,
            other => panic!("{text:?} parsed as a package: {other:?}"),
        }
    }

    fn package(text: &str) -> (Template, Vec<LineSpec>) {
        match parse_builtin(text).unwrap() {
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
        let e = parse_builtin("SPX DEC26 95%/105% CS DO 4200").unwrap_err();
        assert_eq!(e.offset, 22, "the barrier token: {e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse_builtin("SPX DEC26 5000 C DO").unwrap_err();
        assert_eq!(e.offset, 19, "a missing level points past the end: {e:?}");
        let e = parse_builtin("SPX DEC26 5000 C XX 4200").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse_builtin("SPX DEC26 5000 C DO abc").unwrap_err();
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
        let e = parse_builtin("").unwrap_err();
        assert_eq!(e.offset, 0);
        assert!(e.message.contains("empty"), "{e:?}");
        let e = parse_builtin("   ").unwrap_err();
        assert!(e.message.contains("empty"), "{e:?}");

        let e = parse_builtin("0 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 0, "{e:?}");
        assert!(e.message.contains("zero"), "{e:?}");

        let e = parse_builtin("SPX").unwrap_err();
        assert_eq!(e.offset, 3, "missing expiry points past the end: {e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse_builtin("SPX DEX26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse_builtin("SPX DEC26 abc C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("strike"), "{e:?}");

        let e = parse_builtin("SPX DEC26 5000").unwrap_err();
        assert_eq!(e.offset, 14, "missing type: {e:?}");
        assert!(e.message.contains("type"), "{e:?}");

        let e = parse_builtin("SPX DEC26 5000 XYZ").unwrap_err();
        assert_eq!(e.offset, 15, "{e:?}");
        assert!(e.message.contains("unknown type"), "{e:?}");
        let e = parse("SPX DEC26 5000 X", &TemplateSet::default()).unwrap_err();
        assert_eq!(
            e.message, "unknown type 'X': C P",
            "an empty set lists only C and P, with no trailing space"
        );

        let e = parse_builtin("SPX DEC26 95%/105%/110% CS").unwrap_err();
        assert_eq!(e.offset, 10, "the strikes token: {e:?}");
        assert!(e.message.contains("CS takes 2 strikes"), "{e:?}");

        let e = parse_builtin("SPX DEC26 5000/5200 C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("1 strike"), "{e:?}");

        let e = parse_builtin("SPX DEC26/MAR27 5000 CS").unwrap_err();
        assert_eq!(e.offset, 4, "the expiries token: {e:?}");
        assert!(e.message.contains("CS takes 1 expiry"), "{e:?}");

        let e = parse_builtin("SPX DEC26 5000 CAL").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("CAL takes 2 expiries"), "{e:?}");

        let e = parse_builtin("SPX DEC26 5000 C extra").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("unexpected"), "{e:?}");

        // A quantity a template weight cannot multiply: the error names
        // the quantity token, not the leg it would have built.
        let e = parse_builtin("9223372036854775807 SPX Z26 4800/5000/5200 FLY").unwrap_err();
        assert_eq!(e.offset, 0, "{e:?}");
        assert!(e.message.contains("out of range"), "{e:?}");
    }

    #[test]
    fn quantities_that_are_not_integers_are_the_underlying() {
        // "1.5" is not an integer, so it is read as an underlying named
        // "1.5" — the grammar has no fractional quantity, and the trader
        // sees the error at the next token.
        let e = parse_builtin("1.5 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");
        // "+3" is a quantity.
        assert_eq!(line("+3 SPX DEC26 5000 C").qty, 3);
    }

    #[test]
    fn an_expiry_renders_as_an_imm_code_a_full_date_or_the_tenor() {
        assert_eq!(render_expiry(&d(2026, 12, 18)), "Z26");
        assert_eq!(render_expiry(&d(2026, 5, 15)), "K26");
        assert_eq!(render_expiry(&d(2026, 12, 20)), "20DEC26");
        assert_eq!(render_expiry(&d(2026, 12, 5)), "05DEC26");
        assert_eq!(render_expiry(&Expiry::Tenor("3m".into())), "3m");
        assert_eq!(
            imm_code(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()),
            Some("Z26".into())
        );
        assert_eq!(
            imm_code(NaiveDate::from_ymd_opt(2026, 12, 11).unwrap()),
            None,
            "the second Friday"
        );
    }

    #[test]
    fn a_strike_renders_without_trailing_zeros() {
        assert_eq!(render_strike(Strike::Absolute(5000.0)), "5000");
        assert_eq!(render_strike(Strike::Absolute(4250.5)), "4250.5");
        assert_eq!(render_strike(Strike::Percent(95.0)), "95%");
        assert_eq!(render_strike(Strike::Percent(102.5)), "102.5%");
    }

    #[test]
    fn a_line_renders_and_round_trips_through_parse() {
        for text in [
            "SPX Z26 5000 C",
            "-5 SPX Z26 95% P",
            "10 NDX 3m 100% C",
            "SPX 20DEC26 5000 C DO 4200",
            "-2 SPX Z26 4800 P UI 5500",
        ] {
            let l = line(text);
            let rendered = render_line(l.qty, &l.instrument);
            assert_eq!(rendered, text, "renders as typed");
            assert_eq!(line(&rendered), l, "round trip");
        }
        // Lower-case input renders upper-case tokens; a qty of 1 is omitted.
        let l = line("1 spx dec26 5000 c");
        assert_eq!(render_line(l.qty, &l.instrument), "SPX Z26 5000 C");
    }

    #[test]
    fn every_template_renders_and_round_trips_through_parse() {
        for text in [
            "-5 SPX Z26 95%/105% CS",
            "SPX Z26 4800/5200 PS",
            "2 SPX Z26 5000 STRD",
            "SPX Z26 4800/5200 STRG",
            "SPX Z26 4800/5200 RR",
            "3 SPX Z26 4800/5000/5200 FLY",
            "SPX Z26/H27 5000 CAL",
        ] {
            let (template, legs) = package(text);
            let pairs: Vec<(i64, &Instrument)> =
                legs.iter().map(|l| (l.qty, &l.instrument)).collect();
            let rendered =
                render_package(builtin().resolve(template.token()).unwrap(), &pairs).expect(text);
            assert_eq!(rendered, text);
            assert_eq!(package(&rendered), (template, legs), "round trip");
        }
    }

    #[test]
    fn a_package_whose_legs_left_the_table_does_not_render_as_the_template() {
        let (template, mut legs) = package("SPX Z26 4800/5200 CS");
        // A 1×2 ratio: the second leg's qty edited (spec §6.4).
        legs[1].qty = -2;
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(
            render_package(builtin().resolve(template.token()).unwrap(), &pairs),
            None
        );
        // Legs on two underlyings.
        let (template, mut legs) = package("SPX Z26 4800/5200 CS");
        if let Instrument::Vanilla(v) = &mut legs[1].instrument {
            v.underlying = "NDX".into();
        }
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(
            render_package(builtin().resolve(template.token()).unwrap(), &pairs),
            None
        );
        // A wrong leg count.
        let (template, legs) = package("SPX Z26 4800/5200 CS");
        let pairs: Vec<(i64, &Instrument)> = legs
            .iter()
            .take(1)
            .map(|l| (l.qty, &l.instrument))
            .collect();
        assert_eq!(
            render_package(builtin().resolve(template.token()).unwrap(), &pairs),
            None
        );
        // Custom has no table, so it never renders as a template.
        assert!(builtin().resolve(Template::CUSTOM.token()).is_none());
        // A barrier leg is never a template leg.
        let b = line("SPX Z26 5000 C DO 4200");
        let c = line("SPX Z26 5200 C");
        assert_eq!(
            render_package(
                builtin().resolve("CS").unwrap(),
                &[(1, &b.instrument), (-1, &c.instrument)]
            ),
            None
        );
    }

    #[test]
    fn a_config_template_parses_and_prints_back() {
        let doc = geode_core::config::LayerDoc::builtin(
            crate::core::PRICER_TEMPLATES_DOC,
            "[CONDOR]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, { weight = -1, strike = 2, kind = \"C\" }, { weight = -1, strike = 3, kind = \"C\" }, { weight = 1, strike = 4, kind = \"C\" } ]\n",
        )
        .unwrap();
        let set = TemplateSet::from_doc(&geode_core::config::merge_docs(
            crate::core::PRICER_TEMPLATES_DOC,
            &[doc],
        ))
        .0;
        let spec = parse("-2 SPX Z26 4800/4900/5100/5200 condor", &set).unwrap();
        let RowSpec::Package { template, legs } = spec else {
            panic!("a package")
        };
        assert_eq!(template, Template::named("CONDOR"));
        assert_eq!(
            legs.iter().map(|l| l.qty).collect::<Vec<_>>(),
            [-2, 2, 2, -2]
        );
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(
            render_package(set.resolve("CONDOR").unwrap(), &pairs).as_deref(),
            Some("-2 SPX Z26 4800/4900/5100/5200 CONDOR")
        );
        assert!(
            parse("SPX Z26 5000 CS", &set).is_err(),
            "only the set's names parse"
        );
        assert!(
            parse("SPX Z26 5000 C", &TemplateSet::default()).is_ok(),
            "C and P need no set"
        );
    }
}
