//! Option-chain XML parsing and writing. `marketData/underlying` and
//! `optionChain/expiry` identify the document (one expiry's chain);
//! `optionChain` carries the forward, spot reference and quote time, and one
//! `quote` per strike with bid, ask and mid vols and bid and ask prices.
//!
//! Vols arrive computed upstream: a quote missing its mid vol is refused,
//! so nothing downstream averages bid and ask. A side is its vol and its
//! price together: a quote may lack one whole side (a one-sided market,
//! carried as NaN in both of that side's columns, since the family has no
//! NULL, and written back with that side's children absent), but half a
//! side or both sides missing is refused naming the strike. Quotes are sorted
//! by strike at parse and a repeated strike is refused. A negative vol or
//! price and a non-positive spot reference are refused; crossed or locked
//! quotes are accepted as market states. Wire tag names
//! remain unverified against any desk XSD.

use chrono::{DateTime, NaiveDate};
use geode_core::document::{
    Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
};
use geode_core::schema::ColumnType;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

/// Built-in dataset name and source configuration `document` value.
pub const NAME: &str = "option_chain";

/// Wire date format shared by the parser and writer, and the canonical
/// spelling of the `expiry` key part.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// The eleven columns, in `document_columns()` order (key, axes, values,
/// document-level attributes). `expiry` is a utf8 key (document keys are
/// text) and `quote_time` utf8 RFC 3339 (document columns cannot be
/// timestamps).
const COLUMNS: &[(&str, ColumnType)] = &[
    ("underlying_ref", ColumnType::Utf8),
    ("expiry", ColumnType::Utf8),
    ("strike", ColumnType::F64),
    ("bid_vol", ColumnType::F64),
    ("ask_vol", ColumnType::F64),
    ("mid_vol", ColumnType::F64),
    ("bid", ColumnType::F64),
    ("ask", ColumnType::F64),
    ("forward", ColumnType::F64),
    ("spot_ref", ColumnType::F64),
    ("quote_time", ColumnType::Utf8),
];

const AXES: [&str; 1] = ["strike"];
const VALUES: [&str; 5] = ["bid_vol", "ask_vol", "mid_vol", "bid", "ask"];
const ATTRIBUTES: [&str; 3] = ["forward", "spot_ref", "quote_time"];

/// A quote's five value children, wire tag to column, in `VALUES` order.
const TAGS: [(&str, &str); 5] = [
    ("bidVol", "bid_vol"),
    ("askVol", "ask_vol"),
    ("midVol", "mid_vol"),
    ("bid", "bid"),
    ("ask", "ask"),
];

#[derive(Debug, Clone, Copy, Default)]
pub struct OptionChainKind;

impl DocumentKind for OptionChainKind {
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

#[derive(Debug, PartialEq, Eq)]
enum Shape {
    Container,
    /// A `<quote>`: opens a fresh accumulator, committed on `</quote>`.
    Quote,
    Leaf(Leaf),
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
enum Leaf {
    Underlying,
    Expiry,
    Forward,
    SpotRef,
    QuoteTime,
    Strike,
    /// One of the five value children in [`TAGS`], by index.
    Field(usize),
}

fn classify(path: &[String]) -> Shape {
    let seg = |i: usize| path[i].as_str();
    match path.len() {
        1 if seg(0) == "marketData" => Shape::Container,
        2 => match seg(1) {
            "underlying" => Shape::Leaf(Leaf::Underlying),
            "optionChain" => Shape::Container,
            _ => Shape::Unknown,
        },
        3 => match (seg(1), seg(2)) {
            ("optionChain", "expiry") => Shape::Leaf(Leaf::Expiry),
            ("optionChain", "forward") => Shape::Leaf(Leaf::Forward),
            ("optionChain", "spotRef") => Shape::Leaf(Leaf::SpotRef),
            ("optionChain", "quoteTime") => Shape::Leaf(Leaf::QuoteTime),
            ("optionChain", "quote") => Shape::Quote,
            _ => Shape::Unknown,
        },
        4 if seg(2) == "quote" => match seg(3) {
            "strike" => Shape::Leaf(Leaf::Strike),
            tag => match TAGS.iter().position(|(t, _)| *t == tag) {
                Some(i) => Shape::Leaf(Leaf::Field(i)),
                None => Shape::Unknown,
            },
        },
        _ => Shape::Unknown,
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

/// An RFC 3339 check that keeps the wire's own text.
fn rfc3339(what: &str, text: &str) -> Result<String, String> {
    DateTime::parse_from_rfc3339(text)
        .map(|_| text.to_string())
        .map_err(|_| format!("{what} '{text}' is not an RFC 3339 time"))
}

fn positive(what: &str, v: f64) -> Result<f64, ParseError> {
    if v > 0.0 {
        Ok(v)
    } else {
        Err(parse_err(format!("{what} {v} is not positive")))
    }
}

/// A vol or price below zero is impossible, not merely odd. Zero and
/// crossed or locked quotes (bid vol above ask vol, mid outside them) are
/// plausible market states and pass.
fn non_negative(what: &str, v: f64) -> Result<f64, ParseError> {
    if v < 0.0 {
        Err(parse_err(format!("{what} {v} is negative")))
    } else {
        Ok(v)
    }
}

/// Open-element path with reusable name buffers. Sibling quotes reuse
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

/// Values of one `<quote>`, committed on `</quote>`.
#[derive(Default)]
struct Quote {
    strike: Option<f64>,
    fields: [Option<f64>; 5],
}

/// The event walk. One pass: each `<quote>` accumulates into `quote` and
/// lands in `rows` as its `</quote>` closes; the rows are sorted by strike
/// once the walk ends.
fn parse(bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
    let mut reader = Reader::from_reader(bytes);
    // See `cvi.rs`: a self-closing element arrives as Start+End instead
    // of a third `Empty` case every arm below would have to repeat.
    reader.config_mut().expand_empty_elements = true;

    let mut stack = PathStack::default();
    let mut unknown_paths: Vec<String> = Vec::new();
    let mut saw_root = false;

    let mut underlying: Option<String> = None;
    let mut expiry: Option<NaiveDate> = None;
    let mut forward: Option<f64> = None;
    let mut spot_ref: Option<f64> = None;
    let mut quote_time: Option<String> = None;
    // The one container this model reads at most once besides the
    // singular leaves, which already record their own presence in their
    // `Option`.
    let mut saw_chain = false;

    let mut quotes_seen = 0usize;
    let mut quote = Quote::default();
    let mut rows: Vec<(f64, [f64; 5])> = Vec::new();

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
                        if path.len() == 2 && path[1] == "optionChain" {
                            if saw_chain {
                                return Err(already_filled("optionChain"));
                            }
                            saw_chain = true;
                        }
                    }
                    Shape::Leaf(_) => {}
                    Shape::Quote => quote = Quote::default(),
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
                    Shape::Leaf(Leaf::Expiry) => {
                        if expiry.is_some() {
                            return Err(already_filled("expiry"));
                        }
                        expiry = Some(date("expiry", trimmed)?);
                    }
                    Shape::Leaf(Leaf::Forward) => {
                        if forward.is_some() {
                            return Err(already_filled("forward"));
                        }
                        forward = Some(positive("forward", number("forward", trimmed)?)?);
                    }
                    Shape::Leaf(Leaf::SpotRef) => {
                        if spot_ref.is_some() {
                            return Err(already_filled("spotRef"));
                        }
                        spot_ref = Some(positive("spotRef", number("spotRef", trimmed)?)?);
                    }
                    Shape::Leaf(Leaf::QuoteTime) => {
                        if quote_time.is_some() {
                            return Err(already_filled("quoteTime"));
                        }
                        quote_time = Some(rfc3339("quoteTime", trimmed).map_err(parse_err)?);
                    }
                    Shape::Leaf(Leaf::Strike) => {
                        if quote.strike.is_some() {
                            return Err(already_filled("strike"));
                        }
                        quote.strike = Some(positive("strike", number("strike", trimmed)?)?);
                    }
                    Shape::Leaf(Leaf::Field(i)) => {
                        let tag = TAGS[i].0;
                        if quote.fields[i].is_some() {
                            return Err(already_filled(tag));
                        }
                        quote.fields[i] = Some(non_negative(tag, number(tag, trimmed)?)?);
                    }
                    Shape::Quote => {
                        // A quote without a strike is named by its 1-based
                        // position; one with a strike is named by it.
                        let n = quotes_seen + 1;
                        let strike = quote
                            .strike
                            .ok_or_else(|| parse_err(format!("quote {n} is missing strike")))?;
                        let [bid_vol, ask_vol, mid_vol, bid, ask] = quote.fields;
                        let missing = |tag: &str| {
                            parse_err(format!("quote at strike {strike} is missing {tag}"))
                        };
                        let mid = mid_vol.ok_or_else(|| missing("midVol"))?;
                        // A side is its vol and its price together: half a
                        // side is a malformed quote, an absent side a
                        // one-sided market (NaN in both its columns; the
                        // family has no NULL).
                        let side = |vol: Option<f64>, price: Option<f64>, vt: &str, pt: &str| match (
                            vol, price,
                        ) {
                            (Some(v), Some(p)) => Ok(Some((v, p))),
                            (None, None) => Ok(None),
                            (Some(_), None) => Err(missing(pt)),
                            (None, Some(_)) => Err(missing(vt)),
                        };
                        let b = side(bid_vol, bid, "bidVol", "bid")?;
                        let a = side(ask_vol, ask, "askVol", "ask")?;
                        if b.is_none() && a.is_none() {
                            return Err(parse_err(format!(
                                "quote at strike {strike} has neither a bid nor an ask"
                            )));
                        }
                        let (bv, bp) = b.unwrap_or((f64::NAN, f64::NAN));
                        let (av, ap) = a.unwrap_or((f64::NAN, f64::NAN));
                        rows.push((strike, [bv, av, mid, bp, ap]));
                        quotes_seen += 1;
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
    let underlying = underlying.ok_or_else(|| parse_err("underlying is missing"))?;
    let expiry = expiry.ok_or_else(|| parse_err("expiry is missing"))?;
    let forward = forward.ok_or_else(|| parse_err("forward is missing"))?;
    let spot_ref = spot_ref.ok_or_else(|| parse_err("spotRef is missing"))?;
    let quote_time = quote_time.ok_or_else(|| parse_err("quoteTime is missing"))?;
    if rows.is_empty() {
        return Err(parse_err("optionChain is missing or has no quote"));
    }
    // Sorted here so every consumer sees ascending strikes (a density
    // over the chain needs them), and a repeated strike is caught.
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some(w) = rows.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(parse_err(format!("strike {} appears twice", w[0].0)));
    }
    let strikes = rows.iter().map(|r| r.0).collect();
    let column = |i: usize| Column::F64(rows.iter().map(|r| r.1[i]).collect());
    Ok(ParsedDocument {
        rows: DocumentRows {
            key: vec![underlying, expiry.format(DATE_FORMAT).to_string()],
            attributes: vec![
                (ATTRIBUTES[0].to_string(), Value::F64(forward)),
                (ATTRIBUTES[1].to_string(), Value::F64(spot_ref)),
                (ATTRIBUTES[2].to_string(), Value::Utf8(quote_time)),
            ],
            axes: vec![(AXES[0].to_string(), Column::F64(strikes))],
            values: VALUES
                .iter()
                .enumerate()
                .map(|(i, name)| (name.to_string(), column(i)))
                .collect(),
        },
        unknown_paths,
    })
}

/// `write` is the parser's exact inverse, so it refuses every shape the
/// parser would never have produced (see `dividend.rs`'s identical rule).
fn check_vocabulary(rows: &DocumentRows) -> Result<(), WriteError> {
    if rows.key.len() != 2 {
        return Err(write_err(format!(
            "option chain has two key parts (underlying, expiry), got {}",
            rows.key.len()
        )));
    }
    let underlying = &rows.key[0];
    if underlying.is_empty() || underlying.trim() != underlying {
        return Err(write_err(format!(
            "option chain underlying '{underlying}' is blank or padded"
        )));
    }
    // chrono parses leniently ("2026-1-5", a leading space or sign), so the
    // key must re-format to itself: parse only ever produces that spelling.
    let canonical = NaiveDate::parse_from_str(&rows.key[1], DATE_FORMAT)
        .map(|d| d.format(DATE_FORMAT).to_string());
    if canonical.as_deref() != Ok(rows.key[1].as_str()) {
        return Err(write_err(format!(
            "option chain expiry key '{}' is not a date (YYYY-MM-DD)",
            rows.key[1]
        )));
    }
    let names = |v: &[(String, Column)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    if names(&rows.axes) != AXES {
        return Err(write_err(format!(
            "option chain axes are [strike], got [{}]",
            names(&rows.axes).join(", ")
        )));
    }
    if names(&rows.values) != VALUES {
        return Err(write_err(format!(
            "option chain values are [{}], got [{}]",
            VALUES.join(", "),
            names(&rows.values).join(", ")
        )));
    }
    let attrs: Vec<String> = rows.attributes.iter().map(|(n, _)| n.clone()).collect();
    if attrs != ATTRIBUTES {
        return Err(write_err(format!(
            "option chain attributes are [{}], got [{}]",
            ATTRIBUTES.join(", "),
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

/// Every chain column is f64; anything else is a shape parse never made.
fn f64_column((name, column): &(String, Column)) -> Result<&[f64], WriteError> {
    match column {
        Column::F64(v) => Ok(v),
        _ => Err(write_err(format!(
            "option chain column '{name}' is f64; the column is not"
        ))),
    }
}

fn write(rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
    check_vocabulary(rows)?;
    let strikes = f64_column(&rows.axes[0])?;
    let mut values: [&[f64]; 5] = [&[]; 5];
    for (slot, column) in values.iter_mut().zip(&rows.values) {
        *slot = f64_column(column)?;
    }
    let (Value::F64(forward), Value::F64(spot_ref), Value::Utf8(quote_time)) = (
        &rows.attributes[0].1,
        &rows.attributes[1].1,
        &rows.attributes[2].1,
    ) else {
        return Err(write_err(
            "option chain attributes are forward (f64), spot_ref (f64) and quote_time (utf8); \
             the values are of other types",
        ));
    };

    let rows_n = strikes.len();
    // A zero-row document is refused rather than published, exactly as
    // `DocumentRows::validate` refuses one.
    if rows_n == 0 {
        return Err(write_err("option chain document has no rows"));
    }
    for (name, column) in VALUES.iter().zip(values) {
        let len = column.len();
        if len != rows_n {
            return Err(write_err(format!(
                "'{name}': expected {rows_n} rows (from 'strike'), got {len}"
            )));
        }
    }
    // The parser sorts and refuses a repeated strike, so it never yields
    // anything but strictly ascending strikes.
    if let Some(w) = strikes.windows(2).find(|w| w[1] <= w[0] || w[1].is_nan()) {
        return Err(write_err(format!(
            "strikes must be strictly ascending ({} follows {})",
            w[1], w[0]
        )));
    }
    // The parser refuses a non-positive strike, forward or spot reference,
    // and a negative vol or price.
    if let Some(k) = strikes.iter().find(|k| **k <= 0.0 || k.is_nan()) {
        return Err(write_err(format!("strike {k} is not positive")));
    }
    if *forward <= 0.0 || forward.is_nan() {
        return Err(write_err(format!("forward {forward} is not positive")));
    }
    if *spot_ref <= 0.0 || spot_ref.is_nan() {
        return Err(write_err(format!("spot_ref {spot_ref} is not positive")));
    }
    // The parser yields a side's vol and price absent together (both NaN),
    // never both sides absent, and always a mid.
    for (i, k) in strikes.iter().enumerate() {
        for (vol, price, vn, pn) in [
            (values[0], values[3], "bid_vol", "bid"),
            (values[1], values[4], "ask_vol", "ask"),
        ] {
            if vol[i].is_nan() != price[i].is_nan() {
                let (has, lacks) = if vol[i].is_nan() { (pn, vn) } else { (vn, pn) };
                return Err(write_err(format!(
                    "quote at strike {k} has {has} without {lacks}"
                )));
            }
        }
        if !values[2][i].is_finite() {
            return Err(write_err(format!(
                "mid_vol {} at strike {k} is not finite",
                values[2][i]
            )));
        }
        if values[0][i].is_nan() && values[1][i].is_nan() {
            return Err(write_err(format!(
                "quote at strike {k} has neither a bid nor an ask"
            )));
        }
    }
    for (name, column) in VALUES.iter().zip(values) {
        if let Some(v) = column.iter().find(|v| **v < 0.0) {
            return Err(write_err(format!("{name} {v} is negative")));
        }
    }
    rfc3339("quote_time", quote_time).map_err(write_err)?;

    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    let leaf = |w: &mut Writer<Vec<u8>>, name: &str, text: &str| -> Result<(), WriteError> {
        let io = |e: std::io::Error| write_err(format!("writing <{name}>: {e}"));
        w.write_event(Event::Start(BytesStart::new(name)))
            .map_err(io)?;
        w.write_event(Event::Text(BytesText::new(text)))
            .map_err(io)?;
        w.write_event(Event::End(BytesEnd::new(name))).map_err(io)
    };
    let io = |e: std::io::Error| write_err(format!("writing the option chain document: {e}"));
    // One text buffer for every number the document carries; see `num_into`.
    let mut buf = String::new();

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("marketData")))
        .map_err(io)?;
    leaf(&mut w, "underlying", &rows.key[0])?;
    w.write_event(Event::Start(BytesStart::new("optionChain")))
        .map_err(io)?;
    leaf(&mut w, "expiry", &rows.key[1])?;
    num_into(&mut buf, "forward", *forward)?;
    leaf(&mut w, "forward", &buf)?;
    num_into(&mut buf, "spotRef", *spot_ref)?;
    leaf(&mut w, "spotRef", &buf)?;
    leaf(&mut w, "quoteTime", quote_time)?;
    for i in 0..rows_n {
        w.write_event(Event::Start(BytesStart::new("quote")))
            .map_err(io)?;
        num_into(&mut buf, "strike", strikes[i])?;
        leaf(&mut w, "strike", &buf)?;
        for ((tag, _), column) in TAGS.iter().zip(values) {
            // An absent side (checked whole above) has no children.
            if column[i].is_nan() {
                continue;
            }
            num_into(&mut buf, tag, column[i])?;
            leaf(&mut w, tag, &buf)?;
        }
        w.write_event(Event::End(BytesEnd::new("quote")))
            .map_err(io)?;
    }
    w.write_event(Event::End(BytesEnd::new("optionChain")))
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

    const DOC: &str = r#"<?xml version="1.0"?>
<marketData>
  <underlying>SPX</underlying>
  <optionChain>
    <expiry>2026-10-16</expiry>
    <forward>7655.5</forward>
    <spotRef>7650</spotRef>
    <quoteTime>2026-09-28T14:00:00Z</quoteTime>
    <quote>
      <strike>7500</strike>
      <bidVol>0.2</bidVol>
      <askVol>0.21</askVol>
      <midVol>0.205</midVol>
      <bid>95.5</bid>
      <ask>99</ask>
    </quote>
    <quote>
      <strike>7600</strike>
      <bidVol>0.19</bidVol>
      <askVol>0.2</askVol>
      <midVol>0.195</midVol>
      <bid>120.25</bid>
      <ask>124.5</ask>
    </quote>
    <quote>
      <strike>7700</strike>
      <bidVol>0.18</bidVol>
      <askVol>0.19</askVol>
      <midVol>0.185</midVol>
      <bid>130</bid>
      <ask>134.75</ask>
    </quote>
  </optionChain>
</marketData>"#;

    fn expected() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX".into(), "2026-10-16".into()],
            attributes: vec![
                ("forward".into(), Value::F64(7655.5)),
                ("spot_ref".into(), Value::F64(7650.0)),
                (
                    "quote_time".into(),
                    Value::Utf8("2026-09-28T14:00:00Z".into()),
                ),
            ],
            axes: vec![("strike".into(), Column::F64(vec![7500.0, 7600.0, 7700.0]))],
            values: vec![
                ("bid_vol".into(), Column::F64(vec![0.2, 0.19, 0.18])),
                ("ask_vol".into(), Column::F64(vec![0.21, 0.2, 0.19])),
                ("mid_vol".into(), Column::F64(vec![0.205, 0.195, 0.185])),
                ("bid".into(), Column::F64(vec![95.5, 120.25, 130.0])),
                ("ask".into(), Column::F64(vec![99.0, 124.5, 134.75])),
            ],
        }
    }

    /// The `n`th (0-based) `<quote>…</quote>` block of `DOC`.
    fn quote_block(n: usize) -> &'static str {
        let mut from = 0;
        for _ in 0..n {
            from += DOC[from..].find("</quote>").unwrap() + "</quote>".len();
        }
        let start = DOC[from..].find("<quote>").unwrap() + from;
        let end = DOC[start..].find("</quote>").unwrap() + start + "</quote>".len();
        &DOC[start..end]
    }

    fn err(doc: &str) -> String {
        OptionChainKind.parse(doc.as_bytes()).unwrap_err().message
    }

    #[test]
    fn the_kind_names_itself_and_its_eleven_columns_in_document_order() {
        assert_eq!(OptionChainKind.name(), "option_chain");
        let names: Vec<&str> = OptionChainKind.columns().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "underlying_ref",
                "expiry",
                "strike",
                "bid_vol",
                "ask_vol",
                "mid_vol",
                "bid",
                "ask",
                "forward",
                "spot_ref",
                "quote_time"
            ]
        );
    }

    #[test]
    fn the_kind_matches_the_option_chain_dataset_it_feeds() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::document::check_kind_against;
        use geode_core::schema::SchemaSpec;
        const DATASET: &str = r#"
[option_chain]
family = "document"
key = ["underlying_ref", "expiry"]
axes = ["strike"]
[option_chain.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[option_chain.columns.expiry]
type = "utf8"
role = "dimension"
[option_chain.columns.strike]
type = "f64"
role = "axis"
[option_chain.columns.bid_vol]
type = "f64"
role = "value"
[option_chain.columns.ask_vol]
type = "f64"
role = "value"
[option_chain.columns.mid_vol]
type = "f64"
role = "value"
[option_chain.columns.bid]
type = "f64"
role = "value"
[option_chain.columns.ask]
type = "f64"
role = "value"
[option_chain.columns.forward]
type = "f64"
role = "attribute"
[option_chain.columns.spot_ref]
type = "f64"
role = "attribute"
[option_chain.columns.quote_time]
type = "utf8"
role = "attribute"
"#;
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DATASET).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(NAME).unwrap();
        assert_eq!(check_kind_against(&OptionChainKind, ds), Ok(()));
        let parsed = OptionChainKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows.validate(ds), Ok(()));
    }

    #[test]
    fn parses_a_well_formed_document() {
        let parsed = OptionChainKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert!(parsed.unknown_paths.is_empty());
    }

    #[test]
    fn round_trips_through_write() {
        let bytes = OptionChainKind.write(&expected()).unwrap();
        assert_eq!(OptionChainKind.parse(&bytes).unwrap().rows, expected());
    }

    #[test]
    fn write_emits_the_documented_shape() {
        let text = String::from_utf8(OptionChainKind.write(&expected()).unwrap()).unwrap();
        let body = DOC.replace(
            r#"<?xml version="1.0"?>"#,
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
        );
        assert_eq!(text, format!("{body}\n"));
    }

    #[test]
    fn quotes_arrive_in_any_order_and_are_sorted_by_strike() {
        let reversed = DOC
            .replace(quote_block(0), "@0")
            .replace(quote_block(2), quote_block(0))
            .replace("@0", quote_block(2));
        assert_ne!(reversed, DOC, "the fixture really was reordered");
        assert_eq!(
            OptionChainKind.parse(reversed.as_bytes()).unwrap().rows,
            expected()
        );
    }

    #[test]
    fn a_duplicate_strike_is_refused_naming_it() {
        let dup = DOC.replace("<strike>7700</strike>", "<strike>7500</strike>");
        assert_eq!(err(&dup), "strike 7500 appears twice");
    }

    #[test]
    fn a_quote_missing_a_value_is_refused_naming_its_strike() {
        for tag in ["bidVol", "askVol", "midVol", "bid", "ask"] {
            let block = quote_block(1);
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            let start = block.find(&open).unwrap();
            let end = block.find(&close).unwrap() + close.len();
            let doc = DOC.replace(block, &block.replacen(&block[start..end], "", 1));
            assert_eq!(
                err(&doc),
                format!("quote at strike 7600 is missing {tag}"),
                "{tag}"
            );
        }
    }

    /// Column-by-column equality where two NaNs are equal: a one-sided
    /// quote carries NaN in its absent side, which `==` never matches.
    fn same_rows(a: &DocumentRows, b: &DocumentRows) -> bool {
        let same_f64 = |x: &[f64], y: &[f64]| {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|(p, q)| p == q || (p.is_nan() && q.is_nan()))
        };
        let same_cols = |x: &[(String, Column)], y: &[(String, Column)]| {
            x.len() == y.len()
                && x.iter().zip(y).all(|((n, c), (m, d))| {
                    n == m
                        && match (c, d) {
                            (Column::F64(c), Column::F64(d)) => same_f64(c, d),
                            (c, d) => c == d,
                        }
                })
        };
        a.key == b.key
            && a.attributes == b.attributes
            && same_cols(&a.axes, &b.axes)
            && same_cols(&a.values, &b.values)
    }

    #[test]
    fn a_quote_may_lack_one_whole_side() {
        // Strike 7500's bid side removed: both children.
        let doc = DOC
            .replacen("<bidVol>0.2</bidVol>", "", 1)
            .replacen("<bid>95.5</bid>", "", 1);
        let parsed = OptionChainKind.parse(doc.as_bytes()).unwrap();
        let rows = &parsed.rows;
        let col = |name: &str| match &rows.values.iter().find(|(n, _)| n == name).unwrap().1 {
            Column::F64(v) => v.clone(),
            _ => unreachable!(),
        };
        let i = 0; // the first quote in DOC is the one edited
        assert!(col("bid_vol")[i].is_nan() && col("bid")[i].is_nan());
        assert!(col("ask_vol")[i].is_finite() && col("mid_vol")[i].is_finite());
    }

    #[test]
    fn half_a_side_or_no_side_is_refused_naming_the_strike() {
        let no_bid_price = DOC.replacen("<bid>95.5</bid>", "", 1);
        assert_eq!(err(&no_bid_price), "quote at strike 7500 is missing bid");
        let no_sides = DOC
            .replacen("<bidVol>0.2</bidVol>", "", 1)
            .replacen("<bid>95.5</bid>", "", 1)
            .replacen("<askVol>0.21</askVol>", "", 1)
            .replacen("<ask>99</ask>", "", 1);
        assert_eq!(
            err(&no_sides),
            "quote at strike 7500 has neither a bid nor an ask"
        );
        let no_mid = DOC.replacen("<midVol>0.205</midVol>", "", 1);
        assert_eq!(err(&no_mid), "quote at strike 7500 is missing midVol");
    }

    #[test]
    fn a_one_sided_quote_round_trips_with_its_side_absent() {
        let mut rows = expected();
        for name in ["bid_vol", "bid"] {
            let Column::F64(v) = &mut rows.values.iter_mut().find(|(n, _)| n == name).unwrap().1
            else {
                unreachable!()
            };
            v[0] = f64::NAN;
        }
        let bytes = OptionChainKind.write(&rows).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(text.matches("<bidVol>").count(), rows.rows() - 1);
        let back = OptionChainKind.parse(&bytes).unwrap().rows;
        assert!(same_rows(&rows, &back), "NaN-aware equality");
    }

    #[test]
    fn write_refuses_half_a_side_and_a_missing_mid() {
        let mut half = expected();
        let Column::F64(v) = &mut half.values[3].1 else {
            unreachable!()
        }; // bid
        v[0] = f64::NAN;
        assert_eq!(
            OptionChainKind.write(&half).unwrap_err().message,
            "quote at strike 7500 has bid_vol without bid"
        );
        let mut half = expected();
        let Column::F64(v) = &mut half.values[1].1 else {
            unreachable!()
        }; // ask_vol
        v[0] = f64::NAN;
        assert_eq!(
            OptionChainKind.write(&half).unwrap_err().message,
            "quote at strike 7500 has ask without ask_vol"
        );
        let mut no_mid = expected();
        let Column::F64(v) = &mut no_mid.values[2].1 else {
            unreachable!()
        }; // mid_vol
        v[0] = f64::NAN;
        assert!(
            OptionChainKind
                .write(&no_mid)
                .unwrap_err()
                .message
                .contains("mid_vol")
        );
    }

    #[test]
    fn a_quote_missing_its_strike_is_refused_by_position() {
        let doc = DOC.replace("<strike>7600</strike>", "");
        assert_eq!(err(&doc), "quote 2 is missing strike");
    }

    #[test]
    fn a_missing_document_field_is_refused() {
        for (element, message) in [
            ("<underlying>SPX</underlying>", "underlying is missing"),
            ("<expiry>2026-10-16</expiry>", "expiry is missing"),
            ("<forward>7655.5</forward>", "forward is missing"),
            ("<spotRef>7650</spotRef>", "spotRef is missing"),
            (
                "<quoteTime>2026-09-28T14:00:00Z</quoteTime>",
                "quoteTime is missing",
            ),
        ] {
            assert_eq!(err(&DOC.replace(element, "")), message, "{element}");
        }
        let no_quotes = (0..3)
            .rev()
            .fold(DOC.to_string(), |d, n| d.replace(quote_block(n), ""));
        assert_eq!(err(&no_quotes), "optionChain is missing or has no quote");
    }

    #[test]
    fn a_bad_expiry_quote_time_strike_or_forward_is_refused() {
        assert_eq!(
            err(&DOC.replace("2026-10-16", "16/10/2026")),
            "expiry '16/10/2026' is not a date (YYYY-MM-DD)"
        );
        assert_eq!(
            err(&DOC.replace("2026-09-28T14:00:00Z", "yesterday")),
            "quoteTime 'yesterday' is not an RFC 3339 time"
        );
        assert_eq!(
            err(&DOC.replace("<strike>7500</strike>", "<strike>0</strike>")),
            "strike 0 is not positive"
        );
        assert_eq!(
            err(&DOC.replace("<forward>7655.5</forward>", "<forward>-1</forward>")),
            "forward -1 is not positive"
        );
        assert!(err(&DOC.replace("<bid>95.5</bid>", "<bid>NaN</bid>")).contains("not finite"));
    }

    /// A negative vol or price and a non-positive spot reference are
    /// refused at parse, naming the wire tag. The five value fields are
    /// taken from the middle quote so the whole tag list is covered.
    #[test]
    fn a_negative_vol_or_price_or_non_positive_spot_ref_is_refused() {
        for (tag, good) in [
            ("bidVol", "0.19"),
            ("askVol", "0.2"),
            ("midVol", "0.195"),
            ("bid", "120.25"),
            ("ask", "124.5"),
        ] {
            let block = quote_block(1);
            let from = format!("<{tag}>{good}</{tag}>");
            assert!(block.contains(&from), "{from}");
            let doc = DOC.replace(
                block,
                &block.replace(&from, &format!("<{tag}>-0.5</{tag}>")),
            );
            assert_eq!(err(&doc), format!("{tag} -0.5 is negative"), "{tag}");
        }
        for spot in ["0", "-7650"] {
            assert_eq!(
                err(&DOC.replace(
                    "<spotRef>7650</spotRef>",
                    &format!("<spotRef>{spot}</spotRef>")
                )),
                format!("spotRef {spot} is not positive")
            );
        }
    }

    /// Zero vols and prices, and crossed or locked quotes (bid vol above
    /// ask vol, mid outside bid and ask) are plausible on the wire and
    /// pass through: the kind refuses impossible values, not odd markets.
    #[test]
    fn a_crossed_or_zero_quote_still_parses() {
        let block = quote_block(1);
        let crossed = block
            .replace("<bidVol>0.19</bidVol>", "<bidVol>0.25</bidVol>")
            .replace("<midVol>0.195</midVol>", "<midVol>0.3</midVol>")
            .replace("<bid>120.25</bid>", "<bid>0</bid>");
        let parsed = OptionChainKind
            .parse(DOC.replace(block, &crossed).as_bytes())
            .unwrap();
        let mut rows = expected();
        rows.values[0].1 = Column::F64(vec![0.2, 0.25, 0.18]);
        rows.values[2].1 = Column::F64(vec![0.205, 0.3, 0.185]);
        rows.values[3].1 = Column::F64(vec![95.5, 0.0, 130.0]);
        assert_eq!(parsed.rows, rows);
        assert_eq!(
            OptionChainKind
                .parse(&OptionChainKind.write(&rows).unwrap())
                .unwrap()
                .rows,
            rows
        );
    }

    #[test]
    fn a_repeated_singleton_is_refused() {
        let twice = DOC.replace(
            "<spotRef>7650</spotRef>",
            "<spotRef>7650</spotRef><spotRef>7651</spotRef>",
        );
        assert!(err(&twice).contains("already filled"), "{}", err(&twice));
    }

    #[test]
    fn an_unknown_element_is_skipped_and_reported_by_path() {
        let doc = DOC.replace(
            "<strike>7600</strike>",
            "<strike>7600</strike><openInterest><n>5</n></openInterest>",
        );
        let parsed = OptionChainKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/optionChain/quote/openInterest"]
        );
    }

    #[test]
    fn write_refuses_what_parse_would_never_produce() {
        let mut one_part = expected();
        one_part.key.pop();
        assert_eq!(
            OptionChainKind.write(&one_part).unwrap_err().message,
            "option chain has two key parts (underlying, expiry), got 1"
        );
        let mut bad_expiry = expected();
        bad_expiry.key[1] = "Oct-26".into();
        assert_eq!(
            OptionChainKind.write(&bad_expiry).unwrap_err().message,
            "option chain expiry key 'Oct-26' is not a date (YYYY-MM-DD)"
        );
        let mut unsorted = expected();
        unsorted.axes[0].1 = Column::F64(vec![7500.0, 7700.0, 7600.0]);
        assert_eq!(
            OptionChainKind.write(&unsorted).unwrap_err().message,
            "strikes must be strictly ascending (7600 follows 7700)"
        );
        let mut equal = expected();
        equal.axes[0].1 = Column::F64(vec![7500.0, 7500.0, 7700.0]);
        assert_eq!(
            OptionChainKind.write(&equal).unwrap_err().message,
            "strikes must be strictly ascending (7500 follows 7500)"
        );
        // chrono parses leniently; the key must be the canonical spelling
        // parse would have produced, or it reads back as a different key.
        for lenient in ["2026-1-5", " 2026-10-16"] {
            let mut rows = expected();
            rows.key[1] = lenient.into();
            assert_eq!(
                OptionChainKind.write(&rows).unwrap_err().message,
                format!("option chain expiry key '{lenient}' is not a date (YYYY-MM-DD)")
            );
        }
        let mut zero_strike = expected();
        zero_strike.axes[0].1 = Column::F64(vec![0.0, 7600.0, 7700.0]);
        assert_eq!(
            OptionChainKind.write(&zero_strike).unwrap_err().message,
            "strike 0 is not positive"
        );
        let mut negative_forward = expected();
        negative_forward.attributes[0].1 = Value::F64(-1.0);
        assert_eq!(
            OptionChainKind
                .write(&negative_forward)
                .unwrap_err()
                .message,
            "forward -1 is not positive"
        );
        let mut padded = expected();
        padded.key[0] = " SPX".into();
        assert_eq!(
            OptionChainKind.write(&padded).unwrap_err().message,
            "option chain underlying ' SPX' is blank or padded"
        );
        let mut bad_time = expected();
        bad_time.attributes[2].1 = Value::Utf8("soon".into());
        assert_eq!(
            OptionChainKind.write(&bad_time).unwrap_err().message,
            "quote_time 'soon' is not an RFC 3339 time"
        );
        let mut empty = expected();
        empty.axes[0].1 = Column::F64(vec![]);
        for v in empty.values.iter_mut() {
            v.1 = Column::F64(vec![]);
        }
        assert_eq!(
            OptionChainKind.write(&empty).unwrap_err().message,
            "option chain document has no rows"
        );
        for (i, name) in VALUES.iter().enumerate() {
            let mut negative = expected();
            let Column::F64(v) = &mut negative.values[i].1 else {
                unreachable!()
            };
            v[1] = -0.5;
            assert_eq!(
                OptionChainKind.write(&negative).unwrap_err().message,
                format!("{name} -0.5 is negative"),
                "{name}"
            );
        }
        for spot in [0.0, -7650.0] {
            let mut rows = expected();
            rows.attributes[1].1 = Value::F64(spot);
            assert_eq!(
                OptionChainKind.write(&rows).unwrap_err().message,
                format!("spot_ref {spot} is not positive")
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn generated_chains_round_trip(
            steps in proptest::collection::vec(1u32..500, 1..80),
            base in 1u32..10_000,
            seed in 0u64..u64::MAX,
        ) {
            // Strictly ascending positive strikes from positive steps; values
            // from a small LCG, finite by construction.
            let mut x = seed | 1;
            let mut draw = || {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 11) as f64 / (1u64 << 53) as f64
            };
            let mut strike = base as f64;
            let mut strikes = Vec::new();
            for s in &steps {
                strike += *s as f64 * 0.5;
                strikes.push(strike);
            }
            let n = strikes.len();
            let mut col = |scale: f64| Column::F64((0..n).map(|_| draw() * scale).collect());
            let mut rows = DocumentRows {
                key: vec!["SPX".into(), "2027-03-19".into()],
                attributes: vec![
                    ("forward".into(), Value::F64(7000.25)),
                    ("spot_ref".into(), Value::F64(6990.5)),
                    ("quote_time".into(), Value::Utf8("2027-01-04T09:30:00+01:00".into())),
                ],
                axes: vec![("strike".into(), Column::F64(strikes))],
                values: vec![
                    ("bid_vol".into(), col(1.0)),
                    ("ask_vol".into(), col(1.0)),
                    ("mid_vol".into(), col(1.0)),
                    ("bid".into(), col(500.0)),
                    ("ask".into(), col(500.0)),
                ],
            };
            // About one quote in eight lacks its bid side, one in eight
            // its ask side; never both.
            for i in 0..n {
                let side = match draw() {
                    d if d < 0.125 => [0, 3],
                    d if d < 0.25 => [1, 4],
                    _ => continue,
                };
                for c in side {
                    let Column::F64(v) = &mut rows.values[c].1 else { unreachable!() };
                    v[i] = f64::NAN;
                }
            }
            let bytes = OptionChainKind.write(&rows).unwrap();
            let parsed = OptionChainKind.parse(&bytes).unwrap();
            proptest::prop_assert!(parsed.unknown_paths.is_empty());
            proptest::prop_assert!(same_rows(&parsed.rows, &rows));
        }
    }
}
