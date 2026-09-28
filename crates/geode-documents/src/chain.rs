//! Option-chain XML parsing and writing. `marketData/underlying` and
//! `optionChain/expiry` identify the document (one expiry's chain);
//! `optionChain` carries the forward, spot reference and quote time, and one
//! `quote` per strike with bid, ask and mid vols and bid and ask prices.
//!
//! Vols arrive computed upstream: a quote missing any of the three vols is
//! refused, so nothing downstream averages bid and ask. Quotes are sorted
//! by strike at parse and a repeated strike is refused. Wire tag names
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
                        spot_ref = Some(number("spotRef", trimmed)?);
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
                        quote.fields[i] = Some(number(tag, trimmed)?);
                    }
                    Shape::Quote => {
                        // Every child is required. A quote without a strike
                        // is named by its 1-based position; one with a
                        // strike is named by it.
                        let n = quotes_seen + 1;
                        let strike = quote
                            .strike
                            .ok_or_else(|| parse_err(format!("quote {n} is missing strike")))?;
                        let mut values = [0.0; 5];
                        for (i, (tag, _)) in TAGS.iter().enumerate() {
                            values[i] = quote.fields[i].ok_or_else(|| {
                                parse_err(format!("quote at strike {strike} is missing {tag}"))
                            })?;
                        }
                        rows.push((strike, values));
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
    if NaiveDate::parse_from_str(&rows.key[1], DATE_FORMAT).is_err() {
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
            let rows = DocumentRows {
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
            let bytes = OptionChainKind.write(&rows).unwrap();
            let parsed = OptionChainKind.parse(&bytes).unwrap();
            proptest::prop_assert!(parsed.unknown_paths.is_empty());
            proptest::prop_assert_eq!(parsed.rows, rows);
        }
    }
}
