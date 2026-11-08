//! Rate-curve XML parsing and writing. `rateCurve/underlying` and
//! `rateCurve/series` identify the document (one curve of one series, such
//! as a repo or box curve, for one underlying); `publishedTime` is the
//! document-level attribute, and one `point` per tenor carries `tenor` and
//! `value` as attributes.
//!
//! Points are sorted by tenor at parse and a repeated tenor is refused
//! naming it, so a curve's tenors are always strictly ascending. A value is
//! any finite number: a rate may be negative. `publishedTime` must be an
//! RFC 3339 time and is stored as its wire text, since document columns
//! cannot be timestamps. Wire element names remain unverified against any
//! desk XSD.

use chrono::DateTime;
use geode_core::document::{
    Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
};
use geode_core::schema::ColumnType;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};

/// Built-in dataset name and source configuration `document` value.
pub const NAME: &str = "rate_curve";

/// The five columns, in `document_columns()` order (key, axes, values,
/// document-level attributes). `published_time` is utf8 RFC 3339 text
/// (document columns cannot be timestamps).
const COLUMNS: &[(&str, ColumnType)] = &[
    ("underlying_ref", ColumnType::Utf8),
    ("series", ColumnType::Utf8),
    ("tenor", ColumnType::F64),
    ("value", ColumnType::F64),
    ("published_time", ColumnType::Utf8),
];

const AXES: [&str; 1] = ["tenor"];
const VALUES: [&str; 1] = ["value"];
const ATTRIBUTES: [&str; 1] = ["published_time"];

/// The root element, and the two attributes of a `point`. Unverified
/// against any desk XSD; corrected here, in code, like the other kinds'
/// element names.
const ROOT: &str = "rateCurve";
const TENOR_ATTR: &str = "tenor";
const VALUE_ATTR: &str = "value";

/// The rate-curve document kind. A unit struct: a kind carries no state,
/// and `builtin_kinds()` hands the same one to every source configured
/// for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateCurveKind;

impl DocumentKind for RateCurveKind {
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

/// Reject repeated singleton fields rather than combining multiple values
/// or silently retaining the last one.
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
/// `dividend.rs`'s `Shape`, with a `Point` whose whole content is its
/// attributes. `Unknown` is the only variant whose subtree is skipped.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// The root: walked into, nothing recorded on its own account.
    Container,
    /// A `<point>`: its `tenor` and `value` attributes are read on `Start`.
    Point,
    /// Text content the model reads, committed on the matching `End`.
    Leaf(Leaf),
    /// Not in the model: reported by path and skipped whole.
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
enum Leaf {
    Underlying,
    Series,
    PublishedTime,
}

/// The model as a path table: matching on depth and the trailing segment
/// keeps this allocation-free on the hot path.
fn classify(path: &[String]) -> Shape {
    let seg = |i: usize| path[i].as_str();
    match path.len() {
        1 if seg(0) == ROOT => Shape::Container,
        2 => match seg(1) {
            "underlying" => Shape::Leaf(Leaf::Underlying),
            "series" => Shape::Leaf(Leaf::Series),
            "publishedTime" => Shape::Leaf(Leaf::PublishedTime),
            "point" => Shape::Point,
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

/// An RFC 3339 check that keeps the wire's own text.
fn rfc3339(what: &str, text: &str) -> Result<String, String> {
    DateTime::parse_from_rfc3339(text)
        .map(|_| text.to_string())
        .map_err(|_| format!("{what} '{text}' is not an RFC 3339 time"))
}

/// A key part parse would have produced: parse trims every leaf, so a
/// blank or padded one would read back as a different key.
fn key_part(what: &str, text: &str) -> Result<(), WriteError> {
    if text.is_empty() || text.trim() != text {
        return Err(write_err(format!(
            "rate curve {what} '{text}' is blank or padded"
        )));
    }
    Ok(())
}

/// Open-element path with reusable name buffers. Sibling points reuse
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

/// Reads one `<point>`'s attributes. Both are required, named by the
/// point's 1-based position in feed order (there is no tenor to name it
/// by until both are read). An unknown attribute is reported by path as
/// `rateCurve/point/@name` and skipped; a repeated attribute is malformed
/// XML, refused by the reader itself.
fn point(
    e: &BytesStart<'_>,
    ordinal: usize,
    stack: &PathStack,
    unknown_paths: &mut Vec<String>,
) -> Result<(f64, f64), ParseError> {
    let mut tenor: Option<f64> = None;
    let mut value: Option<f64> = None;
    for attr in e.attributes() {
        let attr = attr.map_err(|e| parse_err(format!("malformed XML: {e}")))?;
        let name = std::str::from_utf8(attr.key.local_name().into_inner())
            .map_err(|e| parse_err(format!("attribute name is not UTF-8: {e}")))?;
        let slot = match name {
            TENOR_ATTR => &mut tenor,
            VALUE_ATTR => &mut value,
            _ => {
                unknown_paths.push(format!("{}/@{name}", stack.joined()));
                continue;
            }
        };
        let text = attr
            .normalized_value(XmlVersion::Explicit1_0)
            .map_err(|e| parse_err(format!("malformed XML: {e}")))?;
        *slot = Some(number(name, text.trim())?);
    }
    let tenor =
        tenor.ok_or_else(|| parse_err(format!("point {ordinal} is missing {TENOR_ATTR}")))?;
    let value =
        value.ok_or_else(|| parse_err(format!("point {ordinal} is missing {VALUE_ATTR}")))?;
    Ok((tenor, value))
}

/// The event walk. One pass: each `<point>`'s attributes land in a
/// `(tenor, value)` row as its `Start` arrives, and the rows are sorted
/// and split into columns once the document closes.
fn parse(bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
    let mut reader = Reader::from_reader(bytes);
    // A self-closing `<point …/>` — the usual spelling — arrives as
    // Start+End instead of a third `Empty` case every arm below would
    // have to repeat.
    reader.config_mut().expand_empty_elements = true;

    let mut stack = PathStack::default();
    let mut unknown_paths: Vec<String> = Vec::new();
    let mut saw_root = false;

    let mut underlying: Option<String> = None;
    let mut series: Option<String> = None;
    let mut published_time: Option<String> = None;
    let mut rows: Vec<(f64, f64)> = Vec::new();

    // The text of whichever element is open, reset by every `Start`.
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
                    if stack.path()[0] != ROOT {
                        return Err(parse_err(format!(
                            "root element '{}' is not '{ROOT}'",
                            stack.path()[0]
                        )));
                    }
                    saw_root = true;
                }
                match classify(stack.path()) {
                    Shape::Container | Shape::Leaf(_) => {}
                    Shape::Point => {
                        rows.push(point(&e, rows.len() + 1, &stack, &mut unknown_paths)?);
                    }
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
                    Shape::Leaf(Leaf::Series) => {
                        if series.is_some() {
                            return Err(already_filled("series"));
                        }
                        if trimmed.is_empty() {
                            return Err(parse_err("series is empty"));
                        }
                        series = Some(trimmed.to_string());
                    }
                    Shape::Leaf(Leaf::PublishedTime) => {
                        if published_time.is_some() {
                            return Err(already_filled("publishedTime"));
                        }
                        published_time =
                            Some(rfc3339("publishedTime", trimmed).map_err(parse_err)?);
                    }
                    // A point was read whole on its `Start`; a container
                    // closing records nothing; and with the stack empty —
                    // a shape quick-xml refuses before we see it — nothing
                    // at all.
                    Shape::Point | Shape::Container | Shape::Unknown => {}
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
        return Err(parse_err(format!("document has no '{ROOT}' element")));
    }
    let underlying = underlying.ok_or_else(|| parse_err("underlying is missing"))?;
    let series = series.ok_or_else(|| parse_err("series is missing"))?;
    let published_time = published_time.ok_or_else(|| parse_err("publishedTime is missing"))?;
    if rows.is_empty() {
        return Err(parse_err(format!("{ROOT} has no point")));
    }
    // Sorted here so every consumer sees ascending tenors (interpolation
    // along the curve needs them), and a repeated tenor is caught.
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some(w) = rows.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(parse_err(format!("tenor {} appears twice", w[0].0)));
    }
    let tenors = rows.iter().map(|r| r.0).collect();
    let values = rows.iter().map(|r| r.1).collect();
    Ok(ParsedDocument {
        rows: DocumentRows {
            key: vec![underlying, series],
            attributes: vec![(ATTRIBUTES[0].to_string(), Value::Utf8(published_time))],
            axes: vec![(AXES[0].to_string(), Column::F64(tenors))],
            values: vec![(VALUES[0].to_string(), Column::F64(values))],
        },
        unknown_paths,
    })
}

/// `write` is the parser's exact inverse, so it refuses every shape it
/// would not itself have produced rather than silently canonicalising
/// one — see `cvi.rs`'s identical `check_vocabulary` for why column
/// *names and order* are checked, not merely their presence.
fn check_vocabulary(rows: &DocumentRows) -> Result<(), WriteError> {
    if rows.key.len() != 2 {
        return Err(write_err(format!(
            "rate curve has two key parts (underlying, series), got {}",
            rows.key.len()
        )));
    }
    key_part("underlying", &rows.key[0])?;
    key_part("series", &rows.key[1])?;
    let names = |v: &[(String, Column)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    if names(&rows.axes) != AXES {
        return Err(write_err(format!(
            "rate curve axes are [{}], got [{}]",
            AXES.join(", "),
            names(&rows.axes).join(", ")
        )));
    }
    if names(&rows.values) != VALUES {
        return Err(write_err(format!(
            "rate curve values are [{}], got [{}]",
            VALUES.join(", "),
            names(&rows.values).join(", ")
        )));
    }
    let attrs: Vec<String> = rows.attributes.iter().map(|(n, _)| n.clone()).collect();
    if attrs != ATTRIBUTES {
        return Err(write_err(format!(
            "rate curve attributes are [{}], got [{}]",
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

fn write(rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
    check_vocabulary(rows)?;
    let Column::F64(tenors) = &rows.axes[0].1 else {
        return Err(write_err(
            "rate curve axis 'tenor' is f64; the column is not",
        ));
    };
    let Column::F64(values) = &rows.values[0].1 else {
        return Err(write_err(
            "rate curve value 'value' is f64; the column is not",
        ));
    };
    let Value::Utf8(published_time) = &rows.attributes[0].1 else {
        return Err(write_err(
            "rate curve attribute 'published_time' is utf8; the value is not",
        ));
    };

    let rows_n = tenors.len();
    // A zero-row document is refused rather than published, exactly as
    // `DocumentRows::validate` refuses one.
    if rows_n == 0 {
        return Err(write_err("rate curve document has no rows"));
    }
    if values.len() != rows_n {
        return Err(write_err(format!(
            "'{}': expected {rows_n} rows (from '{}'), got {}",
            VALUES[0],
            AXES[0],
            values.len()
        )));
    }
    // The parser sorts and refuses a repeated tenor, so it never yields
    // anything but strictly ascending tenors.
    if let Some(w) = tenors.windows(2).find(|w| w[1] <= w[0] || w[1].is_nan()) {
        return Err(write_err(format!(
            "tenors must be strictly ascending ({} follows {})",
            w[1], w[0]
        )));
    }
    rfc3339(ATTRIBUTES[0], published_time).map_err(write_err)?;

    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    let leaf = |w: &mut Writer<Vec<u8>>, name: &str, text: &str| -> Result<(), WriteError> {
        let io = |e: std::io::Error| write_err(format!("writing <{name}>: {e}"));
        w.write_event(Event::Start(BytesStart::new(name)))
            .map_err(io)?;
        w.write_event(Event::Text(BytesText::new(text)))
            .map_err(io)?;
        w.write_event(Event::End(BytesEnd::new(name))).map_err(io)
    };
    let io = |e: std::io::Error| write_err(format!("writing the rate curve document: {e}"));
    // Two text buffers, one per attribute, reused across every point.
    let mut tenor_buf = String::new();
    let mut value_buf = String::new();

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new(ROOT)))
        .map_err(io)?;
    leaf(&mut w, "underlying", &rows.key[0])?;
    leaf(&mut w, "series", &rows.key[1])?;
    leaf(&mut w, "publishedTime", published_time)?;
    for i in 0..rows_n {
        num_into(&mut tenor_buf, AXES[0], tenors[i])?;
        num_into(&mut value_buf, VALUES[0], values[i])?;
        let point = BytesStart::new("point").with_attributes([
            (TENOR_ATTR, tenor_buf.as_str()),
            (VALUE_ATTR, value_buf.as_str()),
        ]);
        w.write_event(Event::Empty(point)).map_err(io)?;
    }
    w.write_event(Event::End(BytesEnd::new(ROOT))).map_err(io)?;
    let mut out = w.into_inner();
    out.push(b'\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::document::{Column, DocumentKind, DocumentRows, Value};

    const DOC: &str = r#"<?xml version="1.0"?>
<rateCurve>
  <underlying>SPX</underlying>
  <series>repo</series>
  <publishedTime>2026-11-08T09:30:00Z</publishedTime>
  <point tenor="0.25" value="0.0412"/>
  <point tenor="1" value="0.0435"/>
  <point tenor="2" value="0.0441"/>
</rateCurve>"#;

    fn expected() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX".into(), "repo".into()],
            attributes: vec![(
                "published_time".into(),
                Value::Utf8("2026-11-08T09:30:00Z".into()),
            )],
            axes: vec![("tenor".into(), Column::F64(vec![0.25, 1.0, 2.0]))],
            values: vec![("value".into(), Column::F64(vec![0.0412, 0.0435, 0.0441]))],
        }
    }

    fn err(doc: &str) -> String {
        RateCurveKind.parse(doc.as_bytes()).unwrap_err().message
    }

    #[test]
    fn the_kind_names_itself_and_its_five_columns_in_document_order() {
        assert_eq!(RateCurveKind.name(), NAME);
        assert_eq!(NAME, "rate_curve");
        assert_eq!(
            RateCurveKind.columns(),
            &[
                ("underlying_ref", ColumnType::Utf8),
                ("series", ColumnType::Utf8),
                ("tenor", ColumnType::F64),
                ("value", ColumnType::F64),
                ("published_time", ColumnType::Utf8),
            ]
        );
    }

    /// The kind's columns must match the demo `[rate_curve]` dataset in
    /// both directions, as source start checks them. The block is the
    /// one in `examples/demo-config/datasets.toml`, verbatim.
    #[test]
    fn the_kind_matches_the_demo_dataset() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::document::check_kind_against;
        use geode_core::schema::SchemaSpec;
        const DATASET: &str = r#"
[rate_curve]
family = "document"
key = ["underlying_ref", "series"]
axes = ["tenor"]

[rate_curve.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[rate_curve.columns.series]
type = "utf8"
role = "dimension"
[rate_curve.columns.tenor]
type = "f64"
role = "axis"
[rate_curve.columns.value]
type = "f64"
role = "value"
[rate_curve.columns.published_time]
type = "utf8"
role = "attribute"                # RFC 3339 text: document columns cannot be timestamps
"#;
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DATASET).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(NAME).unwrap();
        assert_eq!(check_kind_against(&RateCurveKind, ds), Ok(()));
        let parsed = RateCurveKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows.validate(ds), Ok(()));
    }

    #[test]
    fn parses_a_well_formed_document() {
        let parsed = RateCurveKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert!(parsed.unknown_paths.is_empty());
    }

    #[test]
    fn a_written_curve_parses_back_to_the_same_rows() {
        let bytes = RateCurveKind.write(&expected()).unwrap();
        assert_eq!(RateCurveKind.parse(&bytes).unwrap().rows, expected());
        // And parse → write → parse is stable from the wire side too.
        let first = RateCurveKind.parse(DOC.as_bytes()).unwrap().rows;
        let second = RateCurveKind
            .parse(&RateCurveKind.write(&first).unwrap())
            .unwrap()
            .rows;
        assert_eq!(first, second);
    }

    /// The wire form itself, pinned: element order, attribute spelling and
    /// two-space indentation. A desk XSD reads this text, not
    /// `DocumentRows`, so a refactor that changes it should have to say so.
    #[test]
    fn write_emits_the_documented_shape() {
        let text = String::from_utf8(RateCurveKind.write(&expected()).unwrap()).unwrap();
        let body = DOC.replace(
            r#"<?xml version="1.0"?>"#,
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
        );
        assert_eq!(text, format!("{body}\n"));
    }

    #[test]
    fn points_are_sorted_by_tenor_and_a_repeated_tenor_is_refused() {
        let shuffled = DOC
            .replace(r#"<point tenor="0.25" value="0.0412"/>"#, "")
            .replace(
                "</rateCurve>",
                r#"  <point tenor="0.25" value="0.0412"/>
</rateCurve>"#,
            );
        assert_eq!(
            RateCurveKind.parse(shuffled.as_bytes()).unwrap().rows,
            expected()
        );

        let repeated = DOC.replace(
            r#"<point tenor="2" value="0.0441"/>"#,
            r#"<point tenor="1" value="0.0441"/>"#,
        );
        assert_eq!(err(&repeated), "tenor 1 appears twice");
    }

    #[test]
    fn a_point_missing_an_attribute_or_carrying_a_bad_number_is_refused() {
        assert_eq!(
            err(&DOC.replace(
                r#"<point tenor="1" value="0.0435"/>"#,
                r#"<point value="0.0435"/>"#
            )),
            "point 2 is missing tenor"
        );
        assert_eq!(
            err(&DOC.replace(
                r#"<point tenor="1" value="0.0435"/>"#,
                r#"<point tenor="1"/>"#
            )),
            "point 2 is missing value"
        );
        assert_eq!(
            err(&DOC.replace(r#"tenor="1""#, r#"tenor="1y""#)),
            "tenor '1y' is not a number"
        );
        assert_eq!(
            err(&DOC.replace(r#"value="0.0435""#, r#"value="NaN""#)),
            "value 'NaN' is not finite"
        );
    }

    #[test]
    fn each_missing_document_level_element_fails() {
        for (needle, what) in [
            ("<underlying>SPX</underlying>", "underlying is missing"),
            ("<series>repo</series>", "series is missing"),
            (
                "<publishedTime>2026-11-08T09:30:00Z</publishedTime>",
                "publishedTime is missing",
            ),
        ] {
            assert_eq!(err(&DOC.replace(needle, "")), what);
        }
        let start = DOC.find("<point").unwrap();
        let end = DOC.rfind("/>").unwrap() + "/>".len();
        let doc = format!("{}{}", &DOC[..start], &DOC[end..]);
        assert_eq!(err(&doc), "rateCurve has no point");
        assert_eq!(
            err(&DOC.replace("2026-11-08T09:30:00Z", "yesterday")),
            "publishedTime 'yesterday' is not an RFC 3339 time"
        );
    }

    /// Every element this model reads at most once is refused on its
    /// second occurrence, never merged or last-wins.
    #[test]
    fn a_repeated_at_most_once_element_is_refused_naming_it() {
        for (element, needle) in [
            ("underlying", "<underlying>SPX</underlying>"),
            ("series", "<series>repo</series>"),
            (
                "publishedTime",
                "<publishedTime>2026-11-08T09:30:00Z</publishedTime>",
            ),
        ] {
            let doc = DOC.replacen(needle, &format!("{needle}{needle}"), 1);
            let message = err(&doc);
            assert!(
                message.contains(element) && message.contains("already"),
                "a second <{element}> is refused: {message}"
            );
        }
    }

    #[test]
    fn an_unknown_element_or_attribute_is_skipped_and_reported_by_path() {
        let doc = DOC
            .replace(
                "<series>repo</series>",
                "<series>repo</series><vendorBlock><point tenor=\"9\" value=\"1\"/></vendorBlock>",
            )
            .replace(
                r#"<point tenor="1" value="0.0435"/>"#,
                r#"<point tenor="1" value="0.0435" source="x"/>"#,
            );
        let parsed = RateCurveKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec![
                "rateCurve/vendorBlock".to_string(),
                "rateCurve/point/@source".to_string()
            ]
        );
    }

    #[test]
    fn a_document_that_is_not_a_rate_curve_fails_naming_its_root() {
        assert_eq!(
            err("<marketData><underlying>SPX</underlying></marketData>"),
            "root element 'marketData' is not 'rateCurve'"
        );
        assert_eq!(err(""), "document has no 'rateCurve' element");
    }

    #[test]
    fn write_refuses_what_parse_would_never_produce() {
        let mut one_part = expected();
        one_part.key.pop();
        assert_eq!(
            RateCurveKind.write(&one_part).unwrap_err().message,
            "rate curve has two key parts (underlying, series), got 1"
        );
        let mut padded = expected();
        padded.key[1] = " repo".into();
        assert_eq!(
            RateCurveKind.write(&padded).unwrap_err().message,
            "rate curve series ' repo' is blank or padded"
        );
        let mut unsorted = expected();
        unsorted.axes[0].1 = Column::F64(vec![0.25, 2.0, 1.0]);
        assert_eq!(
            RateCurveKind.write(&unsorted).unwrap_err().message,
            "tenors must be strictly ascending (1 follows 2)"
        );
        let mut equal = expected();
        equal.axes[0].1 = Column::F64(vec![0.25, 1.0, 1.0]);
        assert_eq!(
            RateCurveKind.write(&equal).unwrap_err().message,
            "tenors must be strictly ascending (1 follows 1)"
        );
        let mut ragged = expected();
        ragged.values[0].1 = Column::F64(vec![0.0412]);
        assert_eq!(
            RateCurveKind.write(&ragged).unwrap_err().message,
            "'value': expected 3 rows (from 'tenor'), got 1"
        );
        let mut empty = expected();
        empty.axes[0].1 = Column::F64(vec![]);
        empty.values[0].1 = Column::F64(vec![]);
        assert_eq!(
            RateCurveKind.write(&empty).unwrap_err().message,
            "rate curve document has no rows"
        );
        let mut swapped = expected();
        swapped.values[0].0 = "rate".into();
        assert_eq!(
            RateCurveKind.write(&swapped).unwrap_err().message,
            "rate curve values are [value], got [rate]"
        );
        let mut bad_time = expected();
        bad_time.attributes[0].1 = Value::Utf8("soon".into());
        assert_eq!(
            RateCurveKind.write(&bad_time).unwrap_err().message,
            "published_time 'soon' is not an RFC 3339 time"
        );
        let mut non_finite = expected();
        non_finite.values[0].1 = Column::F64(vec![0.0412, f64::INFINITY, 0.0441]);
        assert_eq!(
            RateCurveKind.write(&non_finite).unwrap_err().message,
            "value inf is not finite"
        );
    }
}
