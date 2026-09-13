//! The CVI kind (spec §6.3): `marketData/underlying`, `cviParams/
//! anchorDate`, `cviParams/spotRef`, `cviParams/nodes/node*` and
//! `cviParams/slices/slice*`, each slice carrying a `term` and one
//! `param` per node, positionally aligned.
//!
//! Hand-written as a `quick_xml::Reader` event walk rather than a serde
//! derive, for three reasons the spec's §6.3 wording turns on. The
//! ragged-slice rule ("both counts in the error") is a cross-element
//! invariant serde has no place to state; the unknown-element rule
//! ("skipped and logged once per (source, path)") needs the *path* of
//! the element that was skipped, which a deserializer's
//! `deny_unknown_fields` does not hand back and its default silence
//! hides; and the whole point of the parse is to land in
//! struct-of-arrays (`DocumentRows`) with nothing allocated per row
//! beyond the columns themselves (PHILOSOPHY §6), where a derive would
//! build a `Vec<Slice>` of row objects first. This file is expected to
//! be *regenerated* from the desk's XSD later, behind these same two
//! functions (roadmap ruling 8) — the shape of the seam is what matters,
//! not that a human typed the walk.

use chrono::NaiveDate;
use geode_core::document::{
    Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
};
use geode_core::schema::ColumnType;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

/// The dataset name this kind feeds, and the name a `[sources.<name>]`
/// spells as its `kind` (spec §6.4).
pub const NAME: &str = "cvi_params";

/// The date form on the wire, both directions. One constant so the
/// writer cannot drift from the parser — the round-trip property (§11)
/// would catch that, but only by failing, and a reader of either half
/// should be able to see the other's format without leaving the line.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// The six columns of spec §6.3's CVI document, in `document_columns()`
/// order (key, axes, values, document-level attributes) — the order
/// `check_kind_against` compares against the dataset at source-open
/// time and the order the staging path expects.
const COLUMNS: &[(&str, ColumnType)] = &[
    ("underlying_ref", ColumnType::Utf8),
    ("term", ColumnType::Date),
    ("node", ColumnType::F64),
    ("param", ColumnType::F64),
    ("anchor_date", ColumnType::Date),
    ("spot_ref", ColumnType::F64),
];

/// The two axis names, in order, and the one value name — named once so
/// the writer's refusals and this module's doc cannot disagree.
const AXES: [&str; 2] = ["term", "node"];
const VALUES: [&str; 1] = ["param"];
const ATTRIBUTES: [&str; 2] = ["anchor_date", "spot_ref"];

/// The CVI document kind. A unit struct: a kind carries no state, and
/// `builtin_kinds()` hands the same one to every source configured for
/// it.
#[derive(Debug, Clone, Copy, Default)]
pub struct CviKind;

impl DocumentKind for CviKind {
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

fn write_err(message: impl Into<String>) -> WriteError {
    WriteError {
        message: message.into(),
    }
}

/// What the model makes of the element now on top of the path stack.
/// `Unknown` is the only variant whose subtree is skipped; every other
/// one is walked, which is why an element at depth *n* can be
/// classified from its own name and its parent's alone — any ancestor
/// that was not recognised took its whole subtree with it.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// Walked into, nothing recorded on its own account.
    Container,
    /// A `<slice>`: opens a fresh (term, params) accumulator.
    Slice,
    /// Text content the model reads, committed on the matching `End`.
    Leaf(Leaf),
    /// Not in the model: reported by path and skipped whole.
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
enum Leaf {
    Underlying,
    AnchorDate,
    SpotRef,
    Node,
    Term,
    Param,
}

/// The model as a path table. Matching on depth and the last two
/// segments (rather than the joined string) keeps this allocation-free
/// on the hot path — `stack.join("/")` happens once per *unknown*
/// element, which is the only place the whole path is needed.
fn classify(path: &[String]) -> Shape {
    let seg = |i: usize| path[i].as_str();
    match path.len() {
        1 if seg(0) == "marketData" => Shape::Container,
        2 => match seg(1) {
            "underlying" => Shape::Leaf(Leaf::Underlying),
            "cviParams" => Shape::Container,
            _ => Shape::Unknown,
        },
        3 => match (seg(1), seg(2)) {
            ("cviParams", "anchorDate") => Shape::Leaf(Leaf::AnchorDate),
            ("cviParams", "spotRef") => Shape::Leaf(Leaf::SpotRef),
            ("cviParams", "nodes") => Shape::Container,
            ("cviParams", "slices") => Shape::Container,
            _ => Shape::Unknown,
        },
        4 => match (seg(2), seg(3)) {
            ("nodes", "node") => Shape::Leaf(Leaf::Node),
            ("slices", "slice") => Shape::Slice,
            _ => Shape::Unknown,
        },
        5 => match (seg(3), seg(4)) {
            ("slice", "term") => Shape::Leaf(Leaf::Term),
            ("slice", "param") => Shape::Leaf(Leaf::Param),
            _ => Shape::Unknown,
        },
        _ => Shape::Unknown,
    }
}

fn number(what: &str, text: &str) -> Result<f64, ParseError> {
    // `str::parse::<f64>` is the exact inverse of `{}` (shortest
    // round-trip) formatting for every finite f64 — the pair the §11
    // property test rests on. `parse` also accepts `inf`/`NaN`, which
    // `write` refuses to emit; a feed that sends one is refused here
    // too, because a NaN param compares unequal to itself and would
    // make every downstream comparison lie.
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

/// The element name, local part only: a desk document carrying a default
/// or prefixed namespace (`<md:marketData>`) is the same document to us,
/// and the reported unknown path reads the same either way.
fn local(name: quick_xml::name::QName<'_>) -> Result<String, ParseError> {
    std::str::from_utf8(name.local_name().into_inner())
        .map(str::to_string)
        .map_err(|e| parse_err(format!("element name is not UTF-8: {e}")))
}

/// Everything a slice accumulates before its `</slice>` commits it.
#[derive(Default)]
struct Slice {
    term: Option<NaiveDate>,
    params: Vec<f64>,
}

/// The event walk. One pass, no intermediate row objects: each slice's
/// params land straight in the output columns as its `</slice>` closes,
/// so peak allocation is the columns plus one slice.
fn parse(bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
    let mut reader = Reader::from_reader(bytes);
    // `<term/>` and `<nodes/>` arrive as Start+End rather than a third
    // `Empty` case every arm below would have to repeat; the empty text
    // that results is refused by the same leaf commit an empty
    // `<term></term>` hits.
    reader.config_mut().expand_empty_elements = true;

    let mut stack: Vec<String> = Vec::new();
    let mut unknown_paths: Vec<String> = Vec::new();
    let mut saw_root = false;

    let mut underlying: Option<String> = None;
    let mut anchor_date: Option<NaiveDate> = None;
    let mut spot_ref: Option<f64> = None;
    let mut nodes: Vec<f64> = Vec::new();

    let mut term_col: Vec<NaiveDate> = Vec::new();
    let mut node_col: Vec<f64> = Vec::new();
    let mut param_col: Vec<f64> = Vec::new();
    let mut slices = 0usize;
    let mut slice = Slice::default();

    // The text of whichever element is open, reset by every `Start`. A
    // container's accumulation (the whitespace between its children) is
    // simply never read, which is what "whitespace-only text between
    // elements is ignored" amounts to.
    let mut text = String::new();

    loop {
        let event = reader
            .read_event()
            .map_err(|e| parse_err(format!("malformed XML: {e}")))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                stack.push(local(e.name())?);
                text.clear();
                if stack.len() == 1 {
                    if stack[0] != "marketData" {
                        return Err(parse_err(format!(
                            "root element '{}' is not 'marketData'",
                            stack[0]
                        )));
                    }
                    saw_root = true;
                }
                match classify(&stack) {
                    Shape::Container | Shape::Leaf(_) => {}
                    Shape::Slice => slice = Slice::default(),
                    Shape::Unknown => {
                        // Reported by path, once per occurrence — the
                        // receiver dedupes per (source, path), so the
                        // parser's job is to say where, not how often
                        // (spec §6.3).
                        unknown_paths.push(stack.join("/"));
                        reader
                            .read_to_end(e.name())
                            .map_err(|err| parse_err(format!("malformed XML: {err}")))?;
                        stack.pop();
                    }
                }
            }
            Event::End(_) => {
                // `check_end_names` is on (quick-xml's default), so this
                // End belongs to the element on top of the stack; an
                // unknown element's End was already eaten by
                // `read_to_end` above.
                let trimmed = text.trim();
                if let Shape::Leaf(leaf) = classify(&stack) {
                    match leaf {
                        Leaf::Underlying => {
                            if trimmed.is_empty() {
                                return Err(parse_err("underlying is empty"));
                            }
                            underlying = Some(trimmed.to_string());
                        }
                        Leaf::AnchorDate => anchor_date = Some(date("anchorDate", trimmed)?),
                        Leaf::SpotRef => spot_ref = Some(number("spotRef", trimmed)?),
                        Leaf::Node => nodes.push(number("node", trimmed)?),
                        Leaf::Term => slice.term = Some(date("term", trimmed)?),
                        Leaf::Param => slice.params.push(number("param", trimmed)?),
                    }
                } else if classify(&stack) == Shape::Slice {
                    let term = slice
                        .term
                        .ok_or_else(|| parse_err(format!("slice {} has no term", slices + 1)))?;
                    // Checked per slice rather than once at the end, so
                    // the message can name the offending term; and before
                    // the count comparison, so an absent `<nodes>` reads
                    // as an absent `<nodes>` rather than as "nodes has 0".
                    if nodes.is_empty() {
                        return Err(parse_err(
                            "nodes is missing or has no node (no node was read before the first slice)",
                        ));
                    }
                    if slice.params.len() != nodes.len() {
                        return Err(parse_err(format!(
                            "slice {} has {} params, nodes has {}",
                            term.format(DATE_FORMAT),
                            slice.params.len(),
                            nodes.len()
                        )));
                    }
                    // Term-major, one row per (term, node) pair: the
                    // alignment IS the document's meaning, so the params
                    // keep their order against the nodes that were read.
                    for (node, param) in nodes.iter().zip(&slice.params) {
                        term_col.push(term);
                        node_col.push(*node);
                        param_col.push(*param);
                    }
                    slices += 1;
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
            // quick-xml 0.41 splits text at every `&…;` and hands the
            // reference back as its own event, so a leaf's text is only
            // whole if these are folded back in. Nothing this kind writes
            // needs one (its text is numbers, dates and an underlying
            // ref), but a vendor's `&amp;` in an underlying name would
            // otherwise silently truncate the key.
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
            // Declaration, comments, processing instructions and the
            // doctype carry nothing this model reads.
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::DocType(_) => {}
            Event::Empty(_) => unreachable!("expand_empty_elements is on"),
        }
    }

    if !saw_root {
        return Err(parse_err("document has no 'marketData' element"));
    }
    let key = underlying.ok_or_else(|| parse_err("underlying is missing"))?;
    let anchor_date = anchor_date.ok_or_else(|| parse_err("anchorDate is missing"))?;
    let spot_ref = spot_ref.ok_or_else(|| parse_err("spotRef is missing"))?;
    if nodes.is_empty() {
        return Err(parse_err("nodes is missing or has no node"));
    }
    if slices == 0 {
        return Err(parse_err("slices is missing or has no slice"));
    }

    Ok(ParsedDocument {
        rows: DocumentRows {
            key: vec![key],
            attributes: vec![
                (ATTRIBUTES[0].to_string(), Value::Date(anchor_date)),
                (ATTRIBUTES[1].to_string(), Value::F64(spot_ref)),
            ],
            axes: vec![
                (AXES[0].to_string(), Column::Date(term_col)),
                (AXES[1].to_string(), Column::F64(node_col)),
            ],
            values: vec![(VALUES[0].to_string(), Column::F64(param_col))],
        },
        unknown_paths,
    })
}

/// The grid `write` found in the rows: the distinct terms in document
/// order and the node list every one of them carries.
struct Grid<'a> {
    terms: Vec<NaiveDate>,
    nodes: &'a [f64],
}

/// `write` is the parser's exact inverse, so it refuses every shape it
/// would not itself have produced rather than silently canonicalising
/// one — a reordered axis or attribute list written out and read back
/// would not equal what went in, and the caller would have no way to
/// know its ordering had been changed underneath it. Column *names* and
/// order are therefore checked, not merely their presence.
fn check_vocabulary(rows: &DocumentRows) -> Result<(), WriteError> {
    if rows.key.len() != 1 {
        return Err(write_err(format!(
            "CVI has one key part (the underlying), got {}",
            rows.key.len()
        )));
    }
    let names = |v: &[(String, Column)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    if names(&rows.axes) != AXES {
        return Err(write_err(format!(
            "CVI axes are [term, node], got [{}]",
            names(&rows.axes).join(", ")
        )));
    }
    if names(&rows.values) != VALUES {
        return Err(write_err(format!(
            "CVI values are [param], got [{}]",
            names(&rows.values).join(", ")
        )));
    }
    let attrs: Vec<String> = rows.attributes.iter().map(|(n, _)| n.clone()).collect();
    if attrs != ATTRIBUTES {
        return Err(write_err(format!(
            "CVI attributes are [anchor_date, spot_ref], got [{}]",
            attrs.join(", ")
        )));
    }
    Ok(())
}

/// Blocks the rows into slices, checking on the way that they really are
/// a full term-major grid. Two distinct failures, because they mean
/// different things to whoever sent the rows: a term appearing in two
/// non-adjacent blocks is a row *order* problem (the document form has
/// one `<slice>` per term, so writing it would split or reorder rows),
/// while a block whose nodes differ from the first's is a *hole* in the
/// grid (the document form has no way to say "this term has no value at
/// this node" — the alignment is positional).
fn grid_of<'a>(terms: &[NaiveDate], nodes: &'a [f64]) -> Result<Grid<'a>, WriteError> {
    let mut distinct: Vec<NaiveDate> = Vec::new();
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    for (i, t) in terms.iter().enumerate() {
        match bounds.last_mut() {
            Some(last) if distinct[distinct.len() - 1] == *t => last.1 = i + 1,
            _ => {
                if distinct.contains(t) {
                    return Err(write_err(format!(
                        "term {} appears in two blocks; CVI rows must be term-major",
                        t.format(DATE_FORMAT)
                    )));
                }
                distinct.push(*t);
                bounds.push((i, i + 1));
            }
        }
    }
    let Some(&(first_start, first_end)) = bounds.first() else {
        return Err(write_err("CVI document has no rows"));
    };
    let first_nodes = &nodes[first_start..first_end];
    for (term, &(start, end)) in distinct.iter().zip(&bounds).skip(1) {
        if &nodes[start..end] != first_nodes {
            return Err(write_err(format!(
                "term {}'s nodes differ from term {}'s ({} against {}); a CVI grid is full and positional",
                term.format(DATE_FORMAT),
                distinct[0].format(DATE_FORMAT),
                end - start,
                first_nodes.len()
            )));
        }
    }
    Ok(Grid {
        terms: distinct,
        nodes: first_nodes,
    })
}

/// A finite number in its shortest round-tripping form. `{}` is not a
/// convenience here: it is the half of the round-trip contract that
/// makes `str::parse::<f64>` exact, and a `{:.6}` "tidier" form would
/// break §11's property for most doubles.
fn num(what: &str, v: f64) -> Result<String, WriteError> {
    if !v.is_finite() {
        return Err(write_err(format!("{what} {v} is not finite")));
    }
    Ok(v.to_string())
}

fn write(rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
    check_vocabulary(rows)?;
    let (Column::Date(terms), Column::F64(nodes)) = (&rows.axes[0].1, &rows.axes[1].1) else {
        return Err(write_err(
            "CVI axes are term (date) and node (f64); the columns are of other types",
        ));
    };
    let Column::F64(params) = &rows.values[0].1 else {
        return Err(write_err("CVI value 'param' is f64; the column is not"));
    };
    let (Value::Date(anchor_date), Value::F64(spot_ref)) =
        (&rows.attributes[0].1, &rows.attributes[1].1)
    else {
        return Err(write_err(
            "CVI attributes are anchor_date (date) and spot_ref (f64); the values are of other types",
        ));
    };
    if nodes.len() != terms.len() {
        return Err(write_err(format!(
            "axis 'node' has {} rows, axis 'term' has {}",
            nodes.len(),
            terms.len()
        )));
    }
    let grid = grid_of(terms, nodes)?;
    let expected = grid.terms.len() * grid.nodes.len();
    // Axes are checked too, not just values: `grid_of` reads the term
    // column, so a term column longer than the grid it describes cannot
    // arise — but a node column of the wrong length can, and so can a
    // value column, which is the shape the caller most often gets wrong.
    for (name, col) in rows.axes.iter().chain(&rows.values) {
        if col.len() != expected {
            return Err(write_err(format!(
                "'{name}': expected {}×{} = {expected} rows, got {}",
                grid.terms.len(),
                grid.nodes.len(),
                col.len()
            )));
        }
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
    let io = |e: std::io::Error| write_err(format!("writing the CVI document: {e}"));

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("marketData")))
        .map_err(io)?;
    leaf(&mut w, "underlying", &rows.key[0])?;
    w.write_event(Event::Start(BytesStart::new("cviParams")))
        .map_err(io)?;
    leaf(
        &mut w,
        "anchorDate",
        &anchor_date.format(DATE_FORMAT).to_string(),
    )?;
    leaf(&mut w, "spotRef", &num("spot_ref", *spot_ref)?)?;
    w.write_event(Event::Start(BytesStart::new("nodes")))
        .map_err(io)?;
    for node in grid.nodes {
        leaf(&mut w, "node", &num("node", *node)?)?;
    }
    w.write_event(Event::End(BytesEnd::new("nodes")))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("slices")))
        .map_err(io)?;
    for (t, term) in grid.terms.iter().enumerate() {
        w.write_event(Event::Start(BytesStart::new("slice")))
            .map_err(io)?;
        leaf(&mut w, "term", &term.format(DATE_FORMAT).to_string())?;
        // One `<param>` per node, in node order: the document's whole
        // meaning is the positional alignment against `<nodes>`, so any
        // other order writes a different surface.
        for n in 0..grid.nodes.len() {
            leaf(
                &mut w,
                "param",
                &num("param", params[t * grid.nodes.len() + n])?,
            )?;
        }
        w.write_event(Event::End(BytesEnd::new("slice")))
            .map_err(io)?;
    }
    w.write_event(Event::End(BytesEnd::new("slices")))
        .map_err(io)?;
    w.write_event(Event::End(BytesEnd::new("cviParams")))
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
  <cviParams>
    <anchorDate>2026-09-12</anchorDate>
    <spotRef>7650</spotRef>
    <nodes><node>-20.0</node><node>-1</node><node>3.5</node></nodes>
    <slices>
      <slice><term>2026-09-18</term><param>-0.34</param><param>0.1</param><param>1.3</param></slice>
      <slice><term>2026-10-16</term><param>-0.3</param><param>0.12</param><param>1.25</param></slice>
    </slices>
  </cviParams>
</marketData>"#;

    fn expected() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-09-12"))),
                ("spot_ref".into(), Value::F64(7650.0)),
            ],
            axes: vec![
                (
                    "term".into(),
                    Column::Date(
                        vec![d("2026-09-18"); 3]
                            .into_iter()
                            .chain(vec![d("2026-10-16"); 3])
                            .collect(),
                    ),
                ),
                (
                    "node".into(),
                    Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5]),
                ),
            ],
            values: vec![(
                "param".into(),
                Column::F64(vec![-0.34, 0.1, 1.3, -0.3, 0.12, 1.25]),
            )],
        }
    }

    /// The whole `<slices>…</slices>` block, so a test can delete or
    /// replace it without a `replace` that also rewrites the closing tag
    /// it just inserted.
    fn slices_block() -> &'static str {
        let start = DOC.find("<slices>").unwrap();
        let end = DOC.find("</slices>").unwrap() + "</slices>".len();
        &DOC[start..end]
    }

    #[test]
    fn the_kind_names_itself_and_its_six_columns_in_document_order() {
        use geode_core::schema::ColumnType;
        assert_eq!(CviKind.name(), NAME);
        assert_eq!(NAME, "cvi_params");
        assert_eq!(
            CviKind.columns(),
            &[
                ("underlying_ref", ColumnType::Utf8),
                ("term", ColumnType::Date),
                ("node", ColumnType::F64),
                ("param", ColumnType::F64),
                ("anchor_date", ColumnType::Date),
                ("spot_ref", ColumnType::F64),
            ]
        );
    }

    /// The §6.4 load-time contract, against the dataset the desk really
    /// declares rather than against a hand-written column list: both
    /// directions of `check_kind_against` agree, so a source pairing this
    /// kind with this dataset opens. If a later task changes either
    /// side's column set, this is the test that says so — the six-column
    /// assertion above only pins what the kind claims, not that anything
    /// declares it.
    #[test]
    fn the_kind_matches_the_cvi_dataset_it_feeds() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::document::check_kind_against;
        use geode_core::schema::SchemaSpec;

        const CVI: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", CVI).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(NAME).expect("the fixture declares it");
        assert_eq!(check_kind_against(&CviKind, ds), Ok(()));
        // And what the parser produces validates against it, which is
        // the other half: `columns()` is a claim, `parse` is the fact.
        let parsed = CviKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows.validate(ds), Ok(()));
    }

    #[test]
    fn builtin_kinds_offers_the_cvi_kind() {
        let kinds = crate::builtin_kinds();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].name(), NAME);
    }

    #[test]
    fn parses_the_sketch_document_term_major() {
        let parsed = CviKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert!(parsed.unknown_paths.is_empty());
    }

    #[test]
    fn a_ragged_slice_fails_with_both_counts() {
        let doc = DOC.replace("<param>1.25</param>", "");
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message
                .contains("slice 2026-10-16 has 2 params, nodes has 3"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_unknown_element_is_skipped_and_reported_by_path() {
        let doc = DOC.replace(
            "<spotRef>7650</spotRef>",
            "<spotRef>7650</spotRef><vendorNote>x</vendorNote>",
        );
        let parsed = CviKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/cviParams/vendorNote".to_string()]
        );
    }

    /// An unknown element's whole subtree is skipped, not walked into —
    /// otherwise a vendor block holding a `<node>` or a `<slice>` of its
    /// own would feed the model rows nobody asked for, and the path
    /// reported would be the child's rather than the block's.
    #[test]
    fn an_unknown_elements_subtree_is_skipped_whole_and_reported_once() {
        let doc = DOC.replace(
            "<spotRef>7650</spotRef>",
            "<spotRef>7650</spotRef><vendorBlock><node>99</node><slice><term>2026-11-20</term></slice></vendorBlock>",
        );
        let parsed = CviKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/cviParams/vendorBlock".to_string()]
        );
    }

    #[test]
    fn each_missing_required_element_fails() {
        for (needle, what) in [
            ("<underlying>SPX.Z</underlying>", "underlying"),
            ("<anchorDate>2026-09-12</anchorDate>", "anchorDate"),
            ("<spotRef>7650</spotRef>", "spotRef"),
            (
                "<nodes><node>-20.0</node><node>-1</node><node>3.5</node></nodes>",
                "nodes",
            ),
            ("<term>2026-09-18</term>", "term"),
        ] {
            let doc = DOC.replace(needle, "");
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(err.message.contains(what), "{what}: {}", err.message);
        }
        // No `<slices>` at all, and a `<slices>` holding no `<slice>`:
        // both are "the document carries no grid".
        for replacement in ["", "<slices></slices>"] {
            let doc = DOC.replace(slices_block(), replacement);
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains("slices"),
                "{replacement:?}: {}",
                err.message
            );
        }
    }

    #[test]
    fn a_non_numeric_param_or_bad_date_fails_naming_the_value() {
        let err = CviKind
            .parse(
                DOC.replace("<param>0.1</param>", "<param>abc</param>")
                    .as_bytes(),
            )
            .unwrap_err();
        assert!(err.message.contains("param 'abc'"), "{}", err.message);
        let err = CviKind
            .parse(DOC.replace("2026-09-18", "18/09/2026").as_bytes())
            .unwrap_err();
        assert!(err.message.contains("term '18/09/2026'"), "{}", err.message);
    }

    #[test]
    fn write_then_parse_round_trips_the_expected_rows() {
        let bytes = CviKind.write(&expected()).unwrap();
        let parsed = CviKind.parse(&bytes).unwrap();
        assert_eq!(parsed.rows, expected());
    }

    /// The wire form itself, pinned: element order, two-space
    /// indentation, and — the part a round trip alone cannot see — that
    /// `-20.0` goes out as `-20` because `{}` is the shortest
    /// round-tripping form, not a fixed number of decimals. A desk XSD
    /// reads this text, not `DocumentRows`, so a refactor that changes it
    /// should have to say so here.
    #[test]
    fn write_emits_the_sketchs_shape() {
        let text = String::from_utf8(CviKind.write(&expected()).unwrap()).unwrap();
        assert_eq!(
            text,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<marketData>
  <underlying>SPX.Z</underlying>
  <cviParams>
    <anchorDate>2026-09-12</anchorDate>
    <spotRef>7650</spotRef>
    <nodes>
      <node>-20</node>
      <node>-1</node>
      <node>3.5</node>
    </nodes>
    <slices>
      <slice>
        <term>2026-09-18</term>
        <param>-0.34</param>
        <param>0.1</param>
        <param>1.3</param>
      </slice>
      <slice>
        <term>2026-10-16</term>
        <param>-0.3</param>
        <param>0.12</param>
        <param>1.25</param>
      </slice>
    </slices>
  </cviParams>
</marketData>
"#
        );
    }

    #[test]
    fn write_refuses_rows_that_are_not_a_full_grid() {
        let mut rows = expected();
        rows.values[0].1 = Column::F64(vec![1.0; 5]);
        assert!(CviKind.write(&rows).unwrap_err().message.contains("6 rows"));
        let mut rows = expected();
        rows.key = vec!["SPX.Z".into(), "NDX.Z".into()];
        assert!(
            CviKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("one key part")
        );
        // A ragged grid whose total happens to be right: two terms, the
        // first with nodes [-20, -1, 3.5] and the second with [-20, -1,
        // -1]. Six rows either way, so only comparing the second block's
        // nodes against the first's can see it.
        let mut rows = expected();
        rows.axes[1].1 = Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, -1.0]);
        let err = CviKind.write(&rows).unwrap_err();
        assert!(err.message.contains("2026-10-16"), "{}", err.message);
        assert!(err.message.contains("nodes"), "{}", err.message);
    }

    /// A term that appears in two separate blocks is not a term-major
    /// grid: writing it would emit that term as two `<slice>`s, which
    /// parses back as one slice's worth of rows in a different order.
    #[test]
    fn write_refuses_a_term_that_is_not_one_contiguous_block() {
        let mut rows = expected();
        rows.axes[0].1 = Column::Date(vec![
            d("2026-09-18"),
            d("2026-10-16"),
            d("2026-09-18"),
            d("2026-10-16"),
            d("2026-09-18"),
            d("2026-10-16"),
        ]);
        let err = CviKind.write(&rows).unwrap_err();
        assert!(err.message.contains("term-major"), "{}", err.message);
    }

    #[test]
    fn write_refuses_a_vocabulary_it_cannot_reproduce() {
        let mut rows = expected();
        rows.axes.swap(0, 1);
        assert!(
            CviKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("axes are [term, node]")
        );
        let mut rows = expected();
        rows.values[0].0 = "vol".into();
        assert!(
            CviKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("values are [param]")
        );
        let mut rows = expected();
        rows.attributes.swap(0, 1);
        assert!(
            CviKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("attributes are [anchor_date, spot_ref]")
        );
        // Non-finite numbers print as `NaN`/`inf`, which no XSD accepts
        // and which `NaN != NaN` makes unverifiable by round trip.
        let mut rows = expected();
        rows.values[0].1 = Column::F64(vec![-0.34, f64::NAN, 1.3, -0.3, 0.12, 1.25]);
        assert!(
            CviKind
                .write(&rows)
                .unwrap_err()
                .message
                .contains("is not finite")
        );
    }

    proptest::proptest! {
        /// Generated grid → written → parsed is the identity, exactly.
        /// The numbers are full-precision doubles on purpose: `{}`
        /// (shortest round-trip) paired with `str::parse::<f64>` is exact
        /// for every finite `f64`, and nothing weaker would be.
        #[test]
        fn generated_grids_round_trip(
            terms in proptest::collection::vec(0u32..2000, 1..8),
            nodes in proptest::collection::vec(-30.0f64..30.0, 1..12),
            seed in 0u64..1000,
        ) {
            let mut terms: Vec<u32> = terms;
            terms.sort_unstable();
            terms.dedup();
            let base = d("2026-01-01");
            let term_dates: Vec<NaiveDate> = terms
                .iter()
                .map(|t| base + chrono::Days::new(*t as u64))
                .collect();
            // A cheap LCG so the params are arbitrary finite doubles
            // without a second strategy whose length would have to track
            // terms × nodes.
            let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut params = Vec::new();
            let mut term_col = Vec::new();
            let mut node_col = Vec::new();
            for t in &term_dates {
                for n in &nodes {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let u = (x >> 11) as f64 / (1u64 << 53) as f64;
                    params.push(u * 4.0 - 2.0);
                    term_col.push(*t);
                    node_col.push(*n);
                }
            }
            let rows = DocumentRows {
                key: vec!["SPX.Z".into()],
                attributes: vec![
                    ("anchor_date".into(), Value::Date(base)),
                    ("spot_ref".into(), Value::F64(7650.0)),
                ],
                axes: vec![
                    ("term".into(), Column::Date(term_col)),
                    ("node".into(), Column::F64(node_col)),
                ],
                values: vec![("param".into(), Column::F64(params))],
            };
            let bytes = CviKind.write(&rows).unwrap();
            let parsed = CviKind.parse(&bytes).unwrap();
            proptest::prop_assert!(parsed.unknown_paths.is_empty());
            proptest::prop_assert_eq!(parsed.rows, rows);
        }
    }
}
