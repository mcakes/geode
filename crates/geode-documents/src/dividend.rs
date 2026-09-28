//! Dividend-schedule XML parsing and writing. `marketData/underlying` identifies
//! the document; `dividends` contains currency, schedule date, and rows with
//! `exDate`, `announcedDate`, `payDate`, `amount`, and `status`.
//!
//! The event reader fills columnar `DocumentRows`, validates the closed status
//! vocabulary, and records paths of skipped unknown elements. Row ids are minted
//! from ex date and same-date ordinal after parsing. An inbound `<id>` is an
//! unknown element; the writer emits no id. Wire tag names remain unverified
//! against the desk's XSD.

use chrono::NaiveDate;
use geode_core::document::{
    Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
};
use geode_core::schema::ColumnType;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::collections::HashMap;

/// Built-in dataset name and source configuration `kind` value.
pub const NAME: &str = "dividend_schedule";

/// Wire date format shared by the parser and writer.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// The nine columns of this document, in `document_columns()` order (key,
/// axes, values, document-level attributes).
const COLUMNS: &[(&str, ColumnType)] = &[
    ("underlying_ref", ColumnType::Utf8),
    ("dividend_id", ColumnType::Utf8),
    ("ex_date", ColumnType::Date),
    ("announced_date", ColumnType::Date),
    ("pay_date", ColumnType::Date),
    ("amount", ColumnType::F64),
    ("status", ColumnType::Utf8),
    ("currency", ColumnType::Utf8),
    ("schedule_date", ColumnType::Date),
];

const AXES: [&str; 1] = ["dividend_id"];
const VALUES: [&str; 5] = ["ex_date", "announced_date", "pay_date", "amount", "status"];
const ATTRIBUTES: [&str; 2] = ["currency", "schedule_date"];

/// Closed status vocabulary accepted by the parser and writer. The market-data
/// panel declares matching choices without depending on this crate; an app
/// composition test checks that the two declarations agree.
pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

/// Wire tags and column names shared by parser and writer. The names remain
/// unverified against the desk's XSD. Row identity is minted separately by
/// `mint_ids`; an inbound `<id>` is reported as an unknown element.
const TAGS: [(&str, &str); 5] = [
    ("exDate", "ex_date"),
    ("announcedDate", "announced_date"),
    ("payDate", "pay_date"),
    ("amount", "amount"),
    ("status", "status"),
];

/// The dividend-schedule document kind. A unit struct: a kind carries no
/// state, and `builtin_kinds()` hands the same one to every source
/// configured for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DividendKind;

impl DocumentKind for DividendKind {
    fn name(&self) -> &'static str {
        NAME
    }

    fn columns(&self) -> &[(&'static str, ColumnType)] {
        COLUMNS
    }

    fn parse(&self, bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
        parse(bytes)
    }

    fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
        write(rows)
    }
}

fn parse_err(message: impl Into<String>) -> ParseError {
    ParseError {
        message: message.into(),
    }
}

/// Reject repeated singleton fields and containers rather than combining
/// multiple values or silently retaining the last one.
fn already_filled(element: &str) -> ParseError {
    parse_err(format!(
        "'{element}' is already filled; it may appear only once"
    ))
}

fn write_err(message: impl Into<String>) -> WriteError {
    WriteError {
        message: message.into(),
    }
}

/// What the model makes of the element now on top of the path stack —
/// `cvi.rs`'s identical `Shape`. `Unknown` is the only variant whose
/// subtree is skipped.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// Walked into, nothing recorded on its own account.
    Container,
    /// A `<dividend>`: opens a fresh accumulator, committed on `</dividend>`.
    Dividend,
    /// Text content the model reads, committed on the matching `End`.
    Leaf(Leaf),
    /// Not in the model: reported by path and skipped whole.
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
enum Leaf {
    Underlying,
    Currency,
    ScheduleDate,
    /// One of the five dividend fields in [`TAGS`], by index.
    Field(usize),
}

/// The model as a path table, exactly `cvi.rs`'s `classify` in shape:
/// matching on depth and the trailing segments keeps this allocation-free
/// on the hot path.
fn classify(path: &[String]) -> Shape {
    let seg = |i: usize| path[i].as_str();
    match path.len() {
        1 if seg(0) == "marketData" => Shape::Container,
        2 => match seg(1) {
            "underlying" => Shape::Leaf(Leaf::Underlying),
            "dividends" => Shape::Container,
            _ => Shape::Unknown,
        },
        3 => match (seg(1), seg(2)) {
            ("dividends", "currency") => Shape::Leaf(Leaf::Currency),
            ("dividends", "scheduleDate") => Shape::Leaf(Leaf::ScheduleDate),
            ("dividends", "dividend") => Shape::Dividend,
            _ => Shape::Unknown,
        },
        4 => match seg(2) {
            "dividend" => match TAGS.iter().position(|(t, _)| *t == seg(3)) {
                Some(i) => Shape::Leaf(Leaf::Field(i)),
                None => Shape::Unknown,
            },
            _ => Shape::Unknown,
        },
        _ => Shape::Unknown,
    }
}

fn number(what: &str, text: &str) -> Result<f64, ParseError> {
    // See `cvi.rs`'s identical helper: `str::parse::<f64>` is the exact
    // inverse of `{}` formatting, which is what the round-trip property
    // rests on; non-finite is refused for the same reason `write` never
    // emits one.
    let v: f64 = text
        .parse()
        .map_err(|_| parse_err(format!("{what} '{text}' is not a number")))?;
    if !v.is_finite() {
        return Err(parse_err(format!("{what} '{text}' is not finite")));
    }
    Ok(v)
}

fn date(what: &str, text: &str) -> Result<NaiveDate, ParseError> {
    NaiveDate::parse_from_str(text, DATE_FORMAT)
        .map_err(|_| parse_err(format!("{what} '{text}' is not a date (YYYY-MM-DD)")))
}

/// Open-element path with reusable name buffers. Sibling dividend rows reuse
/// capacity rather than allocating a name for each element.
#[derive(Default)]
struct PathStack {
    names: Vec<String>,
    depth: usize,
}

impl PathStack {
    fn push(&mut self, name: quick_xml::name::QName<'_>) -> Result<(), ParseError> {
        let text = std::str::from_utf8(name.local_name().into_inner())
            .map_err(|e| parse_err(format!("element name is not UTF-8: {e}")))?;
        if self.depth == self.names.len() {
            self.names.push(String::new());
        }
        let slot = &mut self.names[self.depth];
        slot.clear();
        slot.push_str(text);
        self.depth += 1;
        Ok(())
    }

    fn pop(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn path(&self) -> &[String] {
        &self.names[..self.depth]
    }

    fn joined(&self) -> String {
        self.path().join("/")
    }
}

/// Everything one `<dividend>` accumulates before its `</dividend>`
/// commits it. Reset with `restart` rather than replaced, in the mould of
/// CVI's `Slice` — though nothing here carries a buffer worth keeping
/// across dividends, since all five fields are scalars.
#[derive(Default)]
struct Dividend {
    ex_date: Option<NaiveDate>,
    announced_date: Option<NaiveDate>,
    pay_date: Option<NaiveDate>,
    amount: Option<f64>,
    status: Option<String>,
}

impl Dividend {
    fn restart(&mut self) {
        *self = Self::default();
    }
}

/// The event walk. One pass, no intermediate row objects: each
/// `<dividend>`'s fields land straight in the output columns as its
/// `</dividend>` closes.
fn parse(bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
    let mut reader = Reader::from_reader(bytes);
    // See `cvi.rs`: a self-closing element arrives as Start+End instead
    // of a third `Empty` case every arm below would have to repeat.
    reader.config_mut().expand_empty_elements = true;

    let mut stack = PathStack::default();
    let mut unknown_paths: Vec<String> = Vec::new();
    let mut saw_root = false;

    let mut underlying: Option<String> = None;
    let mut currency: Option<String> = None;
    let mut schedule_date: Option<NaiveDate> = None;

    let mut ex_date_col: Vec<NaiveDate> = Vec::new();
    let mut announced_date_col: Vec<NaiveDate> = Vec::new();
    let mut pay_date_col: Vec<NaiveDate> = Vec::new();
    let mut amount_col: Vec<f64> = Vec::new();
    let mut status_col: Vec<String> = Vec::new();

    let mut dividends_seen = 0usize;
    let mut dividend = Dividend::default();
    // The one container this model reads at most once besides the
    // singular leaves, which already record their own presence in their
    // `Option`.
    let mut saw_dividends = false;

    // The text of whichever element is open, reset by every `Start` — see
    // `cvi.rs`'s identical field for why a container's own whitespace is
    // simply never read.
    let mut text = String::new();

    loop {
        let event = reader
            .read_event()
            .map_err(|e| parse_err(format!("malformed XML: {e}")))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                stack.push(e.name())?;
                text.clear();
                if stack.path().len() == 1 {
                    if stack.path()[0] != "marketData" {
                        return Err(parse_err(format!(
                            "root element '{}' is not 'marketData'",
                            stack.path()[0]
                        )));
                    }
                    saw_root = true;
                }
                match classify(stack.path()) {
                    Shape::Container => {
                        let path = stack.path();
                        if path.len() == 2 && path[1] == "dividends" {
                            if saw_dividends {
                                return Err(already_filled("dividends"));
                            }
                            saw_dividends = true;
                        }
                    }
                    Shape::Leaf(_) => {}
                    Shape::Dividend => dividend.restart(),
                    Shape::Unknown => {
                        // Reported by path, once per occurrence — the
                        // receiver dedupes per (source, path).
                        unknown_paths.push(stack.joined());
                        reader
                            .read_to_end(e.name())
                            .map_err(|err| parse_err(format!("malformed XML: {err}")))?;
                        stack.pop();
                    }
                }
            }
            Event::End(_) => {
                let trimmed = text.trim();
                match classify(stack.path()) {
                    Shape::Leaf(Leaf::Underlying) => {
                        if underlying.is_some() {
                            return Err(already_filled("underlying"));
                        }
                        if trimmed.is_empty() {
                            return Err(parse_err("underlying is empty"));
                        }
                        underlying = Some(trimmed.to_string());
                    }
                    Shape::Leaf(Leaf::Currency) => {
                        if currency.is_some() {
                            return Err(already_filled("currency"));
                        }
                        currency = Some(trimmed.to_string());
                    }
                    Shape::Leaf(Leaf::ScheduleDate) => {
                        if schedule_date.is_some() {
                            return Err(already_filled("scheduleDate"));
                        }
                        schedule_date = Some(date("scheduleDate", trimmed)?);
                    }
                    Shape::Leaf(Leaf::Field(i)) => {
                        let (tag, _column) = TAGS[i];
                        match tag {
                            "exDate" => {
                                if dividend.ex_date.is_some() {
                                    return Err(already_filled(tag));
                                }
                                dividend.ex_date = Some(date(tag, trimmed)?);
                            }
                            "announcedDate" => {
                                if dividend.announced_date.is_some() {
                                    return Err(already_filled(tag));
                                }
                                dividend.announced_date = Some(date(tag, trimmed)?);
                            }
                            "payDate" => {
                                if dividend.pay_date.is_some() {
                                    return Err(already_filled(tag));
                                }
                                dividend.pay_date = Some(date(tag, trimmed)?);
                            }
                            "amount" => {
                                if dividend.amount.is_some() {
                                    return Err(already_filled(tag));
                                }
                                dividend.amount = Some(number(tag, trimmed)?);
                            }
                            "status" => {
                                if dividend.status.is_some() {
                                    return Err(already_filled(tag));
                                }
                                if !STATUSES.contains(&trimmed) {
                                    return Err(parse_err(format!(
                                        "status '{trimmed}' is not one of [{}]",
                                        STATUSES.join(", ")
                                    )));
                                }
                                dividend.status = Some(trimmed.to_string());
                            }
                            _ => unreachable!("TAGS names only the five recognised children"),
                        }
                    }
                    Shape::Dividend => {
                        // Every child is required, named by the row's
                        // 1-based position — there is no id to name it by
                        // until `mint_ids` runs, after every row has
                        // committed.
                        let row = dividends_seen + 1;
                        let ex_date = dividend.ex_date.ok_or_else(|| {
                            parse_err(format!("dividend {row} is missing exDate"))
                        })?;
                        let announced_date = dividend.announced_date.ok_or_else(|| {
                            parse_err(format!("dividend {row} is missing announcedDate"))
                        })?;
                        let pay_date = dividend.pay_date.ok_or_else(|| {
                            parse_err(format!("dividend {row} is missing payDate"))
                        })?;
                        let amount = dividend.amount.ok_or_else(|| {
                            parse_err(format!("dividend {row} is missing amount"))
                        })?;
                        let status = dividend.status.clone().ok_or_else(|| {
                            parse_err(format!("dividend {row} is missing status"))
                        })?;
                        ex_date_col.push(ex_date);
                        announced_date_col.push(announced_date);
                        pay_date_col.push(pay_date);
                        amount_col.push(amount);
                        status_col.push(status);
                        dividends_seen += 1;
                    }
                    // A container closing, and — with the stack empty, a
                    // shape quick-xml refuses before we see it — nothing
                    // at all.
                    Shape::Container | Shape::Unknown => {}
                }
                stack.pop();
            }
            Event::Text(t) => text.push_str(
                &t.xml10_content()
                    .map_err(|e| parse_err(format!("text is not valid UTF-8: {e}")))?,
            ),
            Event::CData(c) => text.push_str(
                &c.decode()
                    .map_err(|e| parse_err(format!("CDATA is not valid UTF-8: {e}")))?,
            ),
            // See `cvi.rs`: quick-xml 0.41 splits text at every `&…;`.
            Event::GeneralRef(r) => {
                let name = r
                    .decode()
                    .map_err(|e| parse_err(format!("entity is not valid UTF-8: {e}")))?;
                match r
                    .resolve_char_ref()
                    .map_err(|e| parse_err(format!("malformed XML: {e}")))?
                {
                    Some(c) => text.push(c),
                    None => match quick_xml::escape::resolve_predefined_entity(&name) {
                        Some(s) => text.push_str(s),
                        None => return Err(parse_err(format!("unknown entity '&{name};'"))),
                    },
                }
            }
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::DocType(_) => {}
            // Unreachable while `expand_empty_elements` is on above — see
            // `cvi.rs`'s identical arm.
            Event::Empty(_) => {
                return Err(parse_err(format!(
                    "internal: a self-closing element under '{}' reached the walk \
                     as an Empty event, but expand_empty_elements is on",
                    stack.joined()
                )));
            }
        }
    }

    if !saw_root {
        return Err(parse_err("document has no 'marketData' element"));
    }
    let key = underlying.ok_or_else(|| parse_err("underlying is missing"))?;
    let currency = currency.ok_or_else(|| parse_err("currency is missing"))?;
    let schedule_date = schedule_date.ok_or_else(|| parse_err("scheduleDate is missing"))?;
    if dividends_seen == 0 {
        return Err(parse_err("dividends is missing or has no dividend"));
    }

    let id_col = mint_ids(&ex_date_col);

    Ok(ParsedDocument {
        rows: DocumentRows {
            key: vec![key],
            attributes: vec![
                (ATTRIBUTES[0].to_string(), Value::Utf8(currency)),
                (ATTRIBUTES[1].to_string(), Value::Date(schedule_date)),
            ],
            axes: vec![(AXES[0].to_string(), Column::Utf8(id_col))],
            values: vec![
                (VALUES[0].to_string(), Column::Date(ex_date_col)),
                (VALUES[1].to_string(), Column::Date(announced_date_col)),
                (VALUES[2].to_string(), Column::Date(pay_date_col)),
                (VALUES[3].to_string(), Column::F64(amount_col)),
                (VALUES[4].to_string(), Column::Utf8(status_col)),
            ],
        },
        unknown_paths,
    })
}

/// Geode's own row identity for a dividend (the wire carries none): the
/// ex date, and `#n` for the `n`th row sharing it in feed order. Stable
/// while a row's ex date and its place among same-day rows are
/// unchanged; `Draft::rebase` refuses edits in a group whose size changed.
/// Never begins `new-`, so it cannot collide with a draft's minted labels.
pub fn mint_ids(ex_dates: &[NaiveDate]) -> Vec<String> {
    let mut seen: HashMap<NaiveDate, usize> = HashMap::new();
    ex_dates
        .iter()
        .map(|d| {
            let n = seen.entry(*d).or_insert(0);
            *n += 1;
            if *n == 1 {
                d.format(DATE_FORMAT).to_string()
            } else {
                format!("{}#{}", d.format(DATE_FORMAT), n)
            }
        })
        .collect()
}

/// `write` is the parser's exact inverse, so it refuses every shape it
/// would not itself have produced rather than silently canonicalising
/// one — see `cvi.rs`'s identical `check_vocabulary` for why column
/// *names and order* are checked, not merely their presence.
fn check_vocabulary(rows: &DocumentRows) -> Result<(), WriteError> {
    if rows.key.len() != 1 {
        return Err(write_err(format!(
            "dividend schedule has one key part (the underlying), got {}",
            rows.key.len()
        )));
    }
    let names = |v: &[(String, Column)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    if names(&rows.axes) != AXES {
        return Err(write_err(format!(
            "dividend schedule axes are [dividend_id], got [{}]",
            names(&rows.axes).join(", ")
        )));
    }
    if names(&rows.values) != VALUES {
        return Err(write_err(format!(
            "dividend schedule values are [{}], got [{}]",
            VALUES.join(", "),
            names(&rows.values).join(", ")
        )));
    }
    let attrs: Vec<String> = rows.attributes.iter().map(|(n, _)| n.clone()).collect();
    if attrs != ATTRIBUTES {
        return Err(write_err(format!(
            "dividend schedule attributes are [currency, schedule_date], got [{}]",
            attrs.join(", ")
        )));
    }
    Ok(())
}

/// A finite number in its shortest round-tripping form — see `cvi.rs`'s
/// identical `num_into` for why `{}` (not a fixed decimal count) is what
/// makes `str::parse::<f64>` exact, and why non-finite is refused.
fn num_into(buf: &mut String, what: &str, v: f64) -> Result<(), WriteError> {
    if !v.is_finite() {
        return Err(write_err(format!("{what} {v} is not finite")));
    }
    buf.clear();
    let _ = std::fmt::Write::write_fmt(buf, format_args!("{v}"));
    Ok(())
}

/// Same buffer discipline for a date — one per row rather than one per
/// document, but the same reason and the same shape as `cvi.rs`'s
/// `date_into`.
fn date_into(buf: &mut String, date: NaiveDate) {
    buf.clear();
    let _ = std::fmt::Write::write_fmt(buf, format_args!("{}", date.format(DATE_FORMAT)));
}

fn write(rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
    check_vocabulary(rows)?;
    let Column::Utf8(ids) = &rows.axes[0].1 else {
        return Err(write_err(
            "dividend schedule axis 'dividend_id' is utf8; the column is not",
        ));
    };
    let Column::Date(ex_dates) = &rows.values[0].1 else {
        return Err(write_err(
            "dividend schedule value 'ex_date' is date; the column is not",
        ));
    };
    let Column::Date(announced_dates) = &rows.values[1].1 else {
        return Err(write_err(
            "dividend schedule value 'announced_date' is date; the column is not",
        ));
    };
    let Column::Date(pay_dates) = &rows.values[2].1 else {
        return Err(write_err(
            "dividend schedule value 'pay_date' is date; the column is not",
        ));
    };
    let Column::F64(amounts) = &rows.values[3].1 else {
        return Err(write_err(
            "dividend schedule value 'amount' is f64; the column is not",
        ));
    };
    let Column::Utf8(statuses) = &rows.values[4].1 else {
        return Err(write_err(
            "dividend schedule value 'status' is utf8; the column is not",
        ));
    };
    let (Value::Utf8(currency), Value::Date(schedule_date)) =
        (&rows.attributes[0].1, &rows.attributes[1].1)
    else {
        return Err(write_err(
            "dividend schedule attributes are currency (utf8) and schedule_date (date); \
             the values are of other types",
        ));
    };

    let rows_n = ids.len();
    // A zero-row document is refused rather than published, exactly as
    // `DocumentRows::validate` refuses one — see that function's doc for
    // why: the archived generation would be held by no rows at all.
    if rows_n == 0 {
        return Err(write_err("dividend schedule document has no rows"));
    }
    for (name, len) in [
        ("dividend_id", ids.len()),
        ("ex_date", ex_dates.len()),
        ("announced_date", announced_dates.len()),
        ("pay_date", pay_dates.len()),
        ("amount", amounts.len()),
        ("status", statuses.len()),
    ] {
        if len != rows_n {
            return Err(write_err(format!(
                "'{name}': expected {rows_n} rows (from 'dividend_id'), got {len}"
            )));
        }
    }
    // The document form has no way to spell a status outside the closed
    // set — an XSD enumeration would refuse it on the wire, so writing
    // one out would produce a document that could not itself be parsed
    // back by a conformant reader.
    if let Some(bad) = statuses.iter().find(|s| !STATUSES.contains(&s.as_str())) {
        return Err(write_err(format!(
            "status '{bad}' is not one of [{}]",
            STATUSES.join(", ")
        )));
    }

    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    let leaf = |w: &mut Writer<Vec<u8>>, name: &str, text: &str| -> Result<(), WriteError> {
        let io = |e: std::io::Error| write_err(format!("writing <{name}>: {e}"));
        w.write_event(Event::Start(BytesStart::new(name)))
            .map_err(io)?;
        w.write_event(Event::Text(BytesText::new(text)))
            .map_err(io)?;
        w.write_event(Event::End(BytesEnd::new(name))).map_err(io)
    };
    let io = |e: std::io::Error| write_err(format!("writing the dividend schedule document: {e}"));
    // One text buffer for every number and date the document carries; see
    // `num_into`/`date_into`.
    let mut buf = String::new();

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("marketData")))
        .map_err(io)?;
    leaf(&mut w, "underlying", &rows.key[0])?;
    w.write_event(Event::Start(BytesStart::new("dividends")))
        .map_err(io)?;
    leaf(&mut w, "currency", currency)?;
    date_into(&mut buf, *schedule_date);
    leaf(&mut w, "scheduleDate", &buf)?;
    for i in 0..rows_n {
        w.write_event(Event::Start(BytesStart::new("dividend")))
            .map_err(io)?;
        // No `<id>` element: the wire carries none, and Geode's own
        // `dividend_id` (minted by `mint_ids` at parse) is internal row
        // identity, never something to publish back upstream.
        date_into(&mut buf, ex_dates[i]);
        leaf(&mut w, "exDate", &buf)?;
        date_into(&mut buf, announced_dates[i]);
        leaf(&mut w, "announcedDate", &buf)?;
        date_into(&mut buf, pay_dates[i]);
        leaf(&mut w, "payDate", &buf)?;
        num_into(&mut buf, "amount", amounts[i])?;
        leaf(&mut w, "amount", &buf)?;
        leaf(&mut w, "status", &statuses[i])?;
        w.write_event(Event::End(BytesEnd::new("dividend")))
            .map_err(io)?;
    }
    w.write_event(Event::End(BytesEnd::new("dividends")))
        .map_err(io)?;
    w.write_event(Event::End(BytesEnd::new("marketData")))
        .map_err(io)?;
    let mut out = w.into_inner();
    out.push(b'\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::document::{Column, DocumentKind, DocumentRows, Value};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    const DOC: &str = r#"<?xml version="1.0"?>
<marketData>
  <underlying>SPX.Z</underlying>
  <dividends>
    <currency>USD</currency>
    <scheduleDate>2026-09-19</scheduleDate>
    <dividend>
      <exDate>2026-10-01</exDate>
      <announcedDate>2026-08-15</announcedDate>
      <payDate>2026-10-15</payDate>
      <amount>1.25</amount>
      <status>declared</status>
    </dividend>
    <dividend>
      <exDate>2027-01-05</exDate>
      <announcedDate>2026-11-01</announcedDate>
      <payDate>2027-01-20</payDate>
      <amount>1.3</amount>
      <status>estimated</status>
    </dividend>
  </dividends>
</marketData>"#;

    fn expected() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("currency".into(), Value::Utf8("USD".into())),
                ("schedule_date".into(), Value::Date(d("2026-09-19"))),
            ],
            axes: vec![(
                "dividend_id".into(),
                Column::Utf8(vec!["2026-10-01".into(), "2027-01-05".into()]),
            )],
            values: vec![
                (
                    "ex_date".into(),
                    Column::Date(vec![d("2026-10-01"), d("2027-01-05")]),
                ),
                (
                    "announced_date".into(),
                    Column::Date(vec![d("2026-08-15"), d("2026-11-01")]),
                ),
                (
                    "pay_date".into(),
                    Column::Date(vec![d("2026-10-15"), d("2027-01-20")]),
                ),
                ("amount".into(), Column::F64(vec![1.25, 1.3])),
                (
                    "status".into(),
                    Column::Utf8(vec!["declared".into(), "estimated".into()]),
                ),
            ],
        }
    }

    /// The whole first `<dividend>…</dividend>` block, so a test can
    /// remove one of its children without also swallowing the sibling
    /// dividend or the closing `</dividends>`.
    fn first_dividend_block() -> &'static str {
        let start = DOC.find("<dividend>").unwrap();
        let end = DOC[start..].find("</dividend>").unwrap() + start + "</dividend>".len();
        &DOC[start..end]
    }

    #[test]
    fn the_kind_names_itself_and_its_nine_columns_in_document_order() {
        assert_eq!(DividendKind.name(), NAME);
        assert_eq!(NAME, "dividend_schedule");
        assert_eq!(
            DividendKind.columns(),
            &[
                ("underlying_ref", ColumnType::Utf8),
                ("dividend_id", ColumnType::Utf8),
                ("ex_date", ColumnType::Date),
                ("announced_date", ColumnType::Date),
                ("pay_date", ColumnType::Date),
                ("amount", ColumnType::F64),
                ("status", ColumnType::Utf8),
                ("currency", ColumnType::Utf8),
                ("schedule_date", ColumnType::Date),
            ]
        );
    }

    /// The kind's columns must match a parsed dividend dataset in both directions,
    /// exercising source-startup validation as well as the kind's own column list.
    #[test]
    fn the_kind_matches_the_dividend_dataset_it_feeds() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::document::check_kind_against;
        use geode_core::schema::SchemaSpec;

        const DIVIDEND_SCHEDULE: &str = r#"
[dividend_schedule]
family = "document"
key = ["underlying_ref"]
axes = ["dividend_id"]
[dividend_schedule.columns.underlying_ref]
type = "utf8"
role = "dimension"
[dividend_schedule.columns.dividend_id]
type = "utf8"
role = "axis"
[dividend_schedule.columns.ex_date]
type = "date"
role = "value"
[dividend_schedule.columns.announced_date]
type = "date"
role = "value"
[dividend_schedule.columns.pay_date]
type = "date"
role = "value"
[dividend_schedule.columns.amount]
type = "f64"
role = "value"
[dividend_schedule.columns.status]
type = "utf8"
role = "value"
[dividend_schedule.columns.currency]
type = "utf8"
role = "attribute"
[dividend_schedule.columns.schedule_date]
type = "date"
role = "attribute"
"#;
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DIVIDEND_SCHEDULE).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(NAME).expect("the fixture declares it");
        assert_eq!(check_kind_against(&DividendKind, ds), Ok(()));
        // And what the parser produces validates against it.
        let parsed = DividendKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows.validate(ds), Ok(()));
    }

    #[test]
    fn builtin_kinds_offers_every_kind() {
        let kinds = crate::builtin_kinds();
        assert_eq!(kinds.len(), 3);
        assert!(kinds.iter().any(|k| k.name() == NAME));
        assert!(kinds.iter().any(|k| k.name() == crate::chain::NAME));
    }

    #[test]
    fn parses_a_well_formed_document() {
        let parsed = DividendKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert!(parsed.unknown_paths.is_empty());
    }

    #[test]
    fn round_trips_through_write() {
        let bytes = DividendKind.write(&expected()).unwrap();
        let parsed = DividendKind.parse(&bytes).unwrap();
        assert_eq!(parsed.rows, expected());
    }

    /// The wire form itself, pinned: element order and two-space
    /// indentation. A desk XSD reads this text, not `DocumentRows`, so a
    /// refactor that changes it should have to say so here.
    #[test]
    fn write_emits_the_documented_shape() {
        let text = String::from_utf8(DividendKind.write(&expected()).unwrap()).unwrap();
        assert_eq!(
            text,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<marketData>
  <underlying>SPX.Z</underlying>
  <dividends>
    <currency>USD</currency>
    <scheduleDate>2026-09-19</scheduleDate>
    <dividend>
      <exDate>2026-10-01</exDate>
      <announcedDate>2026-08-15</announcedDate>
      <payDate>2026-10-15</payDate>
      <amount>1.25</amount>
      <status>declared</status>
    </dividend>
    <dividend>
      <exDate>2027-01-05</exDate>
      <announcedDate>2026-11-01</announcedDate>
      <payDate>2027-01-20</payDate>
      <amount>1.3</amount>
      <status>estimated</status>
    </dividend>
  </dividends>
</marketData>
"#
        );
    }

    /// Reads `values[0]` (`ex_date`) back out as plain dates, for a test to
    /// hand to `mint_ids` and compare against the axis the parser actually
    /// produced.
    fn ex_dates_of(rows: &DocumentRows) -> Vec<NaiveDate> {
        let Column::Date(dates) = &rows.values[0].1 else {
            panic!("values[0] is ex_date, a date column");
        };
        dates.clone()
    }

    #[test]
    fn mint_ids_numbers_same_day_rows_in_feed_order() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            mint_ids(&[
                d("2026-09-18"),
                d("2026-12-18"),
                d("2026-09-18"),
                d("2026-09-18")
            ]),
            vec!["2026-09-18", "2026-12-18", "2026-09-18#2", "2026-09-18#3"],
        );
    }

    #[test]
    fn parse_mints_ids_from_ex_dates() {
        let doc = DividendKind.parse(DOC.as_bytes()).unwrap();
        let Column::Utf8(ids) = &doc.rows.axes[0].1 else {
            panic!()
        };
        // DOC's two rows: their exDates, in feed order.
        assert_eq!(ids, &mint_ids(&ex_dates_of(&doc.rows)));
        assert!(ids.iter().all(|id| !id.starts_with("new-")));
    }

    #[test]
    fn an_inbound_id_is_an_unknown_element() {
        let doc = DOC.replacen("<exDate>", "<id>X</id><exDate>", 1);
        let parsed = DividendKind.parse(doc.as_bytes()).unwrap();
        assert!(
            parsed
                .unknown_paths
                .iter()
                .any(|p| p.ends_with("dividend/id")),
            "{:?}",
            parsed.unknown_paths
        );
    }

    #[test]
    fn write_emits_no_id_even_for_a_minted_label() {
        let mut rows = expected();
        rows.axes[0].1 = Column::Utf8(vec!["new-1".into(), "new-2".into()]);
        let xml = String::from_utf8(DividendKind.write(&rows).unwrap()).unwrap();
        assert!(!xml.contains("<id>"), "{xml}");
    }

    #[test]
    fn parse_write_parse_is_stable_with_ids_reminted() {
        let first = DividendKind.parse(DOC.as_bytes()).unwrap().rows;
        let bytes = DividendKind.write(&first).unwrap();
        let second = DividendKind.parse(&bytes).unwrap().rows;
        assert_eq!(first, second);
    }

    #[test]
    fn an_unknown_status_is_refused_naming_it() {
        let doc = DOC.replace("<status>declared</status>", "<status>voided</status>");
        let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("voided") && err.message.contains("status"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_malformed_date_fails_naming_the_value() {
        let doc = DOC.replace("2026-10-01", "01/10/2026");
        let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("exDate '01/10/2026'"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_malformed_amount_fails_naming_the_value() {
        let doc = DOC.replace("<amount>1.25</amount>", "<amount>abc</amount>");
        let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
        assert!(err.message.contains("amount 'abc'"), "{}", err.message);
    }

    /// Every element this model reads at most once is refused on its
    /// second occurrence, never merged or last-wins — see `cvi.rs`'s
    /// identical test for why a merge is the worse outcome.
    #[test]
    fn a_repeated_at_most_once_element_is_refused_naming_it() {
        for (element, needle, second) in [
            (
                "underlying",
                "<dividends>",
                "<underlying>NDX.Z</underlying><dividends>",
            ),
            (
                "currency",
                "<currency>USD</currency>",
                "<currency>USD</currency><currency>EUR</currency>",
            ),
            (
                "scheduleDate",
                "<scheduleDate>2026-09-19</scheduleDate>",
                "<scheduleDate>2026-09-19</scheduleDate><scheduleDate>2026-09-20</scheduleDate>",
            ),
        ] {
            let doc = DOC.replacen(needle, second, 1);
            let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains(element) && err.message.contains("already"),
                "a second <{element}> is refused: {}",
                err.message
            );
        }

        // A whole second `<dividends>` block, appended after the first —
        // the container itself, not one of its leaves.
        let extra = format!(
            "</dividends>\n  <dividends><currency>EUR</currency><scheduleDate>2026-09-20</scheduleDate>{}</dividends>",
            first_dividend_block()
        );
        let doc = DOC.replacen("</dividends>", &extra, 1);
        let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("dividends") && err.message.contains("already"),
            "{}",
            err.message
        );
    }

    /// A `<dividend>` missing any of its six children is refused naming
    /// which one — never filled from a sibling dividend or left NULL
    /// (the long form has no NULL here).
    #[test]
    fn a_dividend_missing_a_child_fails_naming_it() {
        for (needle, what) in [
            ("<exDate>2026-10-01</exDate>", "exDate"),
            ("<announcedDate>2026-08-15</announcedDate>", "announcedDate"),
            ("<payDate>2026-10-15</payDate>", "payDate"),
            ("<amount>1.25</amount>", "amount"),
            ("<status>declared</status>", "status"),
        ] {
            let doc = DOC.replace(needle, "");
            let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains("dividend 1") && err.message.contains(what),
                "{what}: {}",
                err.message
            );
        }
        // A `<dividends>` holding no `<dividend>` at all (currency and
        // scheduleDate present): "the document carries no schedule",
        // distinct from a missing `<dividends>` container, which is
        // caught by the document-level checks instead (see
        // `each_missing_document_level_element_fails`).
        let start = DOC.find("<dividend>").unwrap();
        let end = DOC.rfind("</dividend>").unwrap() + "</dividend>".len();
        let doc = format!("{}{}", &DOC[..start], &DOC[end..]);
        let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("dividends") && err.message.contains("no dividend"),
            "{}",
            err.message
        );
    }

    #[test]
    fn each_missing_document_level_element_fails() {
        for (needle, what) in [
            ("<underlying>SPX.Z</underlying>", "underlying"),
            ("<currency>USD</currency>", "currency"),
            ("<scheduleDate>2026-09-19</scheduleDate>", "scheduleDate"),
        ] {
            let doc = DOC.replace(needle, "");
            let err = DividendKind.parse(doc.as_bytes()).unwrap_err();
            assert!(err.message.contains(what), "{what}: {}", err.message);
        }
    }

    #[test]
    fn an_unknown_element_is_skipped_and_reported_by_path() {
        let doc = DOC.replace(
            "<currency>USD</currency>",
            "<currency>USD</currency><vendorNote>x</vendorNote>",
        );
        let parsed = DividendKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/dividends/vendorNote".to_string()]
        );
    }

    /// An unknown element's whole subtree is skipped, not walked into —
    /// otherwise a vendor block holding a `<dividend>` of its own would
    /// feed the model rows nobody asked for.
    #[test]
    fn an_unknown_elements_subtree_is_skipped_whole_and_reported_once() {
        let doc = DOC.replace(
            "<currency>USD</currency>",
            "<currency>USD</currency><vendorBlock><dividend><id>X</id></dividend></vendorBlock>",
        );
        let parsed = DividendKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/dividends/vendorBlock".to_string()]
        );
    }

    #[test]
    fn a_document_that_is_not_market_data_fails_naming_its_root() {
        let err = DividendKind
            .parse(b"<vols><underlying>SPX.Z</underlying></vols>")
            .unwrap_err();
        assert!(
            err.message
                .contains("root element 'vols' is not 'marketData'"),
            "{}",
            err.message
        );
        let err = DividendKind.parse(b"").unwrap_err();
        assert!(
            err.message.contains("no 'marketData' element"),
            "{}",
            err.message
        );
    }

    #[test]
    fn write_refuses_an_unknown_status() {
        let mut rows = expected();
        rows.values[4].1 = Column::Utf8(vec!["declared".into(), "voided".into()]);
        let err = DividendKind.write(&rows).unwrap_err();
        assert!(
            err.message.contains("voided") && err.message.contains("status"),
            "{}",
            err.message
        );
    }

    #[test]
    fn write_refuses_a_zero_row_document() {
        let mut rows = expected();
        rows.axes[0].1 = Column::Utf8(vec![]);
        rows.values[0].1 = Column::Date(vec![]);
        rows.values[1].1 = Column::Date(vec![]);
        rows.values[2].1 = Column::Date(vec![]);
        rows.values[3].1 = Column::F64(vec![]);
        rows.values[4].1 = Column::Utf8(vec![]);
        let err = DividendKind.write(&rows).unwrap_err();
        assert!(err.message.contains("no rows"), "{}", err.message);
    }

    #[test]
    fn write_refuses_mismatched_column_lengths() {
        let mut rows = expected();
        rows.values[3].1 = Column::F64(vec![1.25]);
        let err = DividendKind.write(&rows).unwrap_err();
        assert!(
            err.message.contains("amount") && err.message.contains("2 rows"),
            "{}",
            err.message
        );
    }

    #[test]
    fn write_refuses_a_vocabulary_it_cannot_reproduce() {
        let mut rows = expected();
        rows.values.swap(0, 1);
        assert!(
            DividendKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("values are [ex_date")
        );
        let mut rows = expected();
        rows.attributes.swap(0, 1);
        assert!(
            DividendKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("attributes are [currency, schedule_date]")
        );
        let mut rows = expected();
        rows.key = vec!["SPX.Z".into(), "NDX.Z".into()];
        assert!(
            DividendKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("one key part")
        );
    }
}
