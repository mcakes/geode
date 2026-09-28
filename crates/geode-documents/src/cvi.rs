//! CVI XML parsing and writing. `marketData/underlying` identifies the document;
//! `cviParams` contains `anchorDate`, `spotRef`, nodes, and term slices. Each
//! slice has one `forward`, `atm`, and `skew`, plus one `param` per node in order.
//!
//! The event reader fills columnar `DocumentRows`, checks slice lengths against
//! the node count, and records paths of skipped unknown elements. Ragged-slice
//! errors include both counts. The subscription receiver owns log deduplication.
//! Wire tag names remain unverified against the desk's XSD.

use chrono::NaiveDate;
use geode_core::document::{
    Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
};
use geode_core::schema::ColumnType;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

/// Built-in dataset name and source configuration `kind` value.
pub const NAME: &str = "cvi_params";

/// Wire date format shared by the parser and writer.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Column vocabulary exposed to source-startup schema validation. The check
/// compares names and types; publication chooses the dataset's column order.
const COLUMNS: &[(&str, ColumnType)] = &[
    ("underlying_ref", ColumnType::Utf8),
    ("term", ColumnType::Date),
    ("node", ColumnType::F64),
    ("param", ColumnType::F64),
    ("forward", ColumnType::F64),
    ("atm", ColumnType::F64),
    ("skew", ColumnType::F64),
    ("anchor_date", ColumnType::Date),
    ("spot_ref", ColumnType::F64),
];

/// The two axis names, in order, and the value names — named once so
/// the writer's refusals and this module's doc cannot disagree.
const AXES: [&str; 2] = ["term", "node"];
const VALUES: [&str; 4] = ["param", "forward", "atm", "skew"];
const ATTRIBUTES: [&str; 2] = ["anchor_date", "spot_ref"];

/// Per-slice wire tags paired with column names. Each value repeats on every
/// node row of its slice. Both parser and writer use this table so they agree
/// on the vocabulary. The tag names remain unverified against the desk's XSD.
const SLICE_VALUES: [(&str, &str); 3] = [("forward", "forward"), ("atm", "atm"), ("skew", "skew")];

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

/// The refusal every at-most-once element shares (`<underlying>`,
/// `<anchorDate>`, `<spotRef>`, `<cviParams>`, `<nodes>`, `<slices>`).
///
/// Refused rather than merged or last-wins, because both silent
/// outcomes are worse than a rejected message: a second `<cviParams>`
/// or `<slices>` repeats every (term, node) pair, and a second
/// `<spotRef>` quietly publishes one of two different spots.
/// `DocumentRows::validate` refuses the repeated-tuple shape too, but
/// only after the parse has thrown away WHICH element the feed sent
/// twice — so this is the report that can name it.
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
    /// One of [`SLICE_VALUES`], by index.
    SliceValue(usize),
}

/// The model as a path table. Matching on depth and the last two
/// segments (rather than on a joined string) keeps this allocation-free
/// on the hot path — `PathStack::joined` runs once per *unknown*
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
            ("slice", tag) => match SLICE_VALUES.iter().position(|(t, _)| *t == tag) {
                Some(i) => Shape::Leaf(Leaf::SliceValue(i)),
                None => Shape::Unknown,
            },
            _ => Shape::Unknown,
        },
        _ => Shape::Unknown,
    }
}

fn number(what: &str, text: &str) -> Result<f64, ParseError> {
    // Parsing inverts the writer's shortest round-trip formatting for finite
    // values. Reject NaN and infinities on input as well as output so stored
    // parameters support meaningful equality and echo comparisons.
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

/// Element path with reusable name buffers at each depth. Sibling elements
/// reuse capacity instead of allocating a name for every parameter row.
/// The depth buffer grows only when a deeper path is encountered.
#[derive(Default)]
struct PathStack {
    names: Vec<String>,
    depth: usize,
}

impl PathStack {
    /// Pushes an element's name, local part only: a desk document
    /// carrying a default or prefixed namespace (`<md:marketData>`) is
    /// the same document to us, and the reported unknown path reads the
    /// same either way.
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

    /// Saturating, so a stray `End` cannot underflow — quick-xml refuses
    /// an unmatched one before we see it (`allow_unmatched_ends` is off),
    /// but a panic here would cost the receiver thread and this costs a
    /// branch.
    fn pop(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn path(&self) -> &[String] {
        &self.names[..self.depth]
    }

    /// Materialize a path only when reporting an unknown element;
    /// classification reads the existing segments.
    fn joined(&self) -> String {
        self.path().join("/")
    }
}

/// Everything a slice accumulates before its `</slice>` commits it.
/// Reset with `restart` rather than replaced, so `params` keeps its
/// buffer across the document's slices.
#[derive(Default)]
struct Slice {
    term: Option<NaiveDate>,
    /// [`SLICE_VALUES`] by index, each read at most once per slice.
    values: [Option<f64>; SLICE_VALUES.len()],
    params: Vec<f64>,
}

impl Slice {
    fn restart(&mut self) {
        self.term = None;
        self.values = [None; SLICE_VALUES.len()];
        self.params.clear();
    }
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

    let mut stack = PathStack::default();
    let mut unknown_paths: Vec<String> = Vec::new();
    let mut saw_root = false;

    let mut underlying: Option<String> = None;
    let mut anchor_date: Option<NaiveDate> = None;
    let mut spot_ref: Option<f64> = None;
    let mut nodes: Vec<f64> = Vec::new();

    let mut term_col: Vec<NaiveDate> = Vec::new();
    let mut node_col: Vec<f64> = Vec::new();
    let mut param_col: Vec<f64> = Vec::new();
    // One column per slice value, filled once per node row of a slice.
    let mut slice_cols: [Vec<f64>; SLICE_VALUES.len()] = Default::default();
    let mut slices = 0usize;
    let mut slice = Slice::default();
    // The containers this model reads at most once. The singular LEAVES
    // need no flag of their own — their `Option` above already records
    // whether one was read — and `<slice>` is tracked by its term
    // instead, two slices being legal only for two different terms.
    let mut saw_cvi_params = false;
    let mut saw_nodes = false;
    let mut saw_slices = false;
    let mut seen_terms: Vec<NaiveDate> = Vec::new();

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
                        // Depth and name tell the singular containers
                        // apart without a second path table. `marketData`
                        // itself is absent from this list: quick-xml
                        // refuses a second root element before the walk
                        // ever sees it.
                        let path = stack.path();
                        let once = match (path.len(), path[path.len() - 1].as_str()) {
                            (2, "cviParams") => Some((&mut saw_cvi_params, "cviParams")),
                            (3, "nodes") => Some((&mut saw_nodes, "nodes")),
                            (3, "slices") => Some((&mut saw_slices, "slices")),
                            _ => None,
                        };
                        if let Some((seen, element)) = once {
                            if *seen {
                                return Err(already_filled(element));
                            }
                            *seen = true;
                        }
                    }
                    Shape::Leaf(_) => {}
                    Shape::Slice => slice.restart(),
                    Shape::Unknown => {
                        // Record each skipped path here; the receiver deduplicates
                        // logging by source and path.
                        unknown_paths.push(stack.joined());
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
                    Shape::Leaf(Leaf::AnchorDate) => {
                        if anchor_date.is_some() {
                            return Err(already_filled("anchorDate"));
                        }
                        anchor_date = Some(date("anchorDate", trimmed)?)
                    }
                    Shape::Leaf(Leaf::SpotRef) => {
                        if spot_ref.is_some() {
                            return Err(already_filled("spotRef"));
                        }
                        spot_ref = Some(number("spotRef", trimmed)?)
                    }
                    Shape::Leaf(Leaf::Node) => nodes.push(number("node", trimmed)?),
                    Shape::Leaf(Leaf::Term) => {
                        // A slice has one term. Reject a repeated leaf rather
                        // than silently publishing under its second date.
                        if slice.term.is_some() {
                            return Err(already_filled("term"));
                        }
                        slice.term = Some(date("term", trimmed)?)
                    }
                    Shape::Leaf(Leaf::Param) => slice.params.push(number("param", trimmed)?),
                    Shape::Leaf(Leaf::SliceValue(i)) => {
                        // The singular-leaf rule inside a slice: a second
                        // `<forward>` would otherwise win silently.
                        let (tag, _) = SLICE_VALUES[i];
                        if slice.values[i].is_some() {
                            return Err(already_filled(tag));
                        }
                        slice.values[i] = Some(number(tag, trimmed)?);
                    }
                    Shape::Slice => {
                        let term = slice.term.ok_or_else(|| {
                            parse_err(format!("slice {} has no term", slices + 1))
                        })?;
                        // Every slice value is required, named by the slice
                        // it is missing from — the `spotRef is missing`
                        // spelling one level down.
                        let mut values = [0.0; SLICE_VALUES.len()];
                        for (i, (tag, _)) in SLICE_VALUES.iter().enumerate() {
                            values[i] = slice.values[i].ok_or_else(|| {
                                parse_err(format!(
                                    "slice {} is missing {tag}",
                                    term.format(DATE_FORMAT)
                                ))
                            })?;
                        }
                        // Two slices for one term are the singular-element
                        // defect one level down: their rows carry the same
                        // (term, node) pairs, which nothing downstream can
                        // tell apart. A linear scan because a document has
                        // a handful of listed expiries, not thousands.
                        if seen_terms.contains(&term) {
                            return Err(parse_err(format!(
                                "slice term {} appears twice",
                                term.format(DATE_FORMAT)
                            )));
                        }
                        seen_terms.push(term);
                        // Checked per slice rather than once at the end,
                        // so the message can name the offending term; and
                        // before the count comparison, so an absent
                        // `<nodes>` reads as an absent `<nodes>` rather
                        // than as "nodes has 0".
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
                        // alignment IS the document's meaning, so the
                        // params keep their order against the nodes that
                        // were read.
                        for (node, param) in nodes.iter().zip(&slice.params) {
                            term_col.push(term);
                            node_col.push(*node);
                            param_col.push(*param);
                            // Repeated per node row: the long form's
                            // spelling of "constant within the slice".
                            for (col, v) in slice_cols.iter_mut().zip(values) {
                                col.push(v);
                            }
                        }
                        slices += 1;
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
            // Unreachable while `expand_empty_elements` is on above (a
            // self-closing element arrives as Start + End instead). An
            // error rather than a panic, because this runs on the
            // receiver thread, where a panic costs the whole message
            // pump and says less than a line naming the invariant.
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
            values: std::iter::once((VALUES[0].to_string(), Column::F64(param_col)))
                .chain(
                    SLICE_VALUES
                        .iter()
                        .zip(slice_cols)
                        .map(|((_, column), col)| (column.to_string(), Column::F64(col))),
                )
                .collect(),
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
            "CVI values are [{}], got [{}]",
            VALUES.join(", "),
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

/// Write a finite number in its shortest round-trip form. Fixed decimal
/// precision would lose values when parsed back; NaN and infinities are refused.
/// The caller reuses the buffer to avoid a String allocation for every row.
fn num_into(buf: &mut String, what: &str, v: f64) -> Result<(), WriteError> {
    if !v.is_finite() {
        return Err(write_err(format!("{what} {v} is not finite")));
    }
    buf.clear();
    // Writing to a `String` is infallible; the `Result` exists only for
    // the general `fmt::Write` shape.
    let _ = std::fmt::Write::write_fmt(buf, format_args!("{v}"));
    Ok(())
}

/// Same buffer discipline for a date — one per slice rather than one per
/// row, but the same reason and the same shape.
fn date_into(buf: &mut String, date: NaiveDate) {
    buf.clear();
    let _ = std::fmt::Write::write_fmt(buf, format_args!("{}", date.format(DATE_FORMAT)));
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
    let mut slice_cols: [&[f64]; SLICE_VALUES.len()] = [&[]; SLICE_VALUES.len()];
    for (i, (_, column)) in SLICE_VALUES.iter().enumerate() {
        let Column::F64(col) = &rows.values[1 + i].1 else {
            return Err(write_err(format!(
                "CVI value '{column}' is f64; the column is not"
            )));
        };
        slice_cols[i] = col;
    }
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
    // One text buffer for every number and date the document carries; see
    // `num_into`.
    let mut buf = String::new();

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("marketData")))
        .map_err(io)?;
    leaf(&mut w, "underlying", &rows.key[0])?;
    w.write_event(Event::Start(BytesStart::new("cviParams")))
        .map_err(io)?;
    date_into(&mut buf, *anchor_date);
    leaf(&mut w, "anchorDate", &buf)?;
    num_into(&mut buf, "spot_ref", *spot_ref)?;
    leaf(&mut w, "spotRef", &buf)?;
    w.write_event(Event::Start(BytesStart::new("nodes")))
        .map_err(io)?;
    for node in grid.nodes {
        num_into(&mut buf, "node", *node)?;
        leaf(&mut w, "node", &buf)?;
    }
    w.write_event(Event::End(BytesEnd::new("nodes")))
        .map_err(io)?;
    w.write_event(Event::Start(BytesStart::new("slices")))
        .map_err(io)?;
    for (t, term) in grid.terms.iter().enumerate() {
        w.write_event(Event::Start(BytesStart::new("slice")))
            .map_err(io)?;
        date_into(&mut buf, *term);
        leaf(&mut w, "term", &buf)?;
        // The slice values, read off the slice's FIRST row and refused
        // when any later row of the slice disagrees — the ragged-slice
        // rule's sibling. The document form says each once per slice, so
        // a slice whose rows carry two forwards has no honest surface;
        // writing the first would silently drop the other.
        let start = t * grid.nodes.len();
        let end = start + grid.nodes.len();
        for ((tag, column), col) in SLICE_VALUES.iter().zip(&slice_cols) {
            let first = col[start];
            if let Some(r) = (start + 1..end).find(|&r| col[r] != first) {
                return Err(write_err(format!(
                    "term {}'s {column} differs within the slice ({first} on node {} against {} on node {}); a slice value is constant across a term's nodes",
                    term.format(DATE_FORMAT),
                    grid.nodes[0],
                    col[r],
                    grid.nodes[r - start]
                )));
            }
            num_into(&mut buf, column, first)?;
            leaf(&mut w, tag, &buf)?;
        }
        // One `<param>` per node, in node order: the document's whole
        // meaning is the positional alignment against `<nodes>`, so any
        // other order writes a different surface.
        for n in 0..grid.nodes.len() {
            num_into(&mut buf, "param", params[t * grid.nodes.len() + n])?;
            leaf(&mut w, "param", &buf)?;
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
      <slice><term>2026-09-18</term><forward>7655.5</forward><atm>0.182</atm><skew>-1.1</skew><param>-0.34</param><param>0.1</param><param>1.3</param></slice>
      <slice><term>2026-10-16</term><forward>7671.25</forward><atm>0.19</atm><skew>-0.95</skew><param>-0.3</param><param>0.12</param><param>1.25</param></slice>
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
            values: vec![
                (
                    "param".into(),
                    Column::F64(vec![-0.34, 0.1, 1.3, -0.3, 0.12, 1.25]),
                ),
                // The slice values, repeated on every node row of their
                // slice (the long form's "constant within a slice").
                (
                    "forward".into(),
                    Column::F64(vec![7655.5, 7655.5, 7655.5, 7671.25, 7671.25, 7671.25]),
                ),
                (
                    "atm".into(),
                    Column::F64(vec![0.182, 0.182, 0.182, 0.19, 0.19, 0.19]),
                ),
                (
                    "skew".into(),
                    Column::F64(vec![-1.1, -1.1, -1.1, -0.95, -0.95, -0.95]),
                ),
            ],
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
    fn the_kind_names_itself_and_its_nine_columns_in_document_order() {
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
                ("forward", ColumnType::F64),
                ("atm", ColumnType::F64),
                ("skew", ColumnType::F64),
                ("anchor_date", ColumnType::Date),
                ("spot_ref", ColumnType::F64),
            ]
        );
        // The wire table and the column table name the same values in
        // the same order — one place a rename must land twice.
        let slice_columns: Vec<&str> = SLICE_VALUES.iter().map(|(_, c)| *c).collect();
        assert_eq!(&VALUES[1..], slice_columns.as_slice());
    }

    /// The kind's column names and types must match a parsed CVI dataset in both
    /// directions. This exercises source-startup validation against a schema,
    /// not only the kind's own column list.
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
[cvi_params.columns.forward]
type = "f64"
role = "value"
[cvi_params.columns.atm]
type = "f64"
role = "value"
[cvi_params.columns.skew]
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
        // The count is `dividend.rs`'s `builtin_kinds_offers_every_kind`.
        let kinds = crate::builtin_kinds();
        assert!(kinds.iter().any(|k| k.name() == NAME));
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

    /// Repeated singular containers are refused by element name. Merging
    /// them could duplicate grid rows and defer the error to validation,
    /// which cannot identify the duplicated XML container.
    #[test]
    fn a_repeated_singular_container_is_refused_naming_it() {
        // A whole second `<cviParams>` carrying only `<slices>`: the
        // narrowest possible duplicate, and exactly the shape a merge
        // hides — the nodes and attributes are not repeated, so nothing
        // but the extra rows would say anything was wrong.
        let extra = format!("</cviParams>\n  <cviParams>{}</cviParams>", slices_block());
        let doc = DOC.replacen("</cviParams>", &extra, 1);
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("cviParams") && err.message.contains("already"),
            "{}",
            err.message
        );

        for (element, block) in [
            ("nodes", "<nodes><node>9</node></nodes>"),
            ("slices", slices_block()),
        ] {
            let doc = DOC.replacen("<slices>", &format!("{block}<slices>"), 1);
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains(element) && err.message.contains("already"),
                "a second <{element}> is refused: {}",
                err.message
            );
        }
    }

    #[test]
    fn a_repeated_singular_leaf_is_refused_naming_it() {
        // Refuse repeated spot values rather than choosing one silently.
        let doc = DOC.replacen(
            "<spotRef>7650</spotRef>",
            "<spotRef>7650</spotRef><spotRef>7700</spotRef>",
            1,
        );
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("spotRef") && err.message.contains("already"),
            "{}",
            err.message
        );

        // Each duplicate goes where the model would actually classify it
        // as that leaf: `<underlying>` is a child of `marketData`,
        // `<anchorDate>` of `cviParams` — inserted at the wrong depth it
        // would merely be an unknown element, and the test would pass for
        // the wrong reason.
        for (element, needle, second) in [
            (
                "underlying",
                "<cviParams>",
                "<underlying>NDX.Z</underlying><cviParams>",
            ),
            (
                "anchorDate",
                "<anchorDate>2026-09-12</anchorDate>",
                "<anchorDate>2026-09-12</anchorDate><anchorDate>2026-09-11</anchorDate>",
            ),
            // A slice value is singular within its slice: a second
            // `<forward>` in one `<slice>` is the same defect one level
            // down, and the same rule.
            (
                "forward",
                "<forward>7655.5</forward>",
                "<forward>7655.5</forward><forward>7700</forward>",
            ),
        ] {
            let doc = DOC.replacen(needle, second, 1);
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains(element) && err.message.contains("already"),
                "a second <{element}> is refused: {}",
                err.message
            );
        }
    }

    /// Two `<slice>` elements for one term are the same defect one level
    /// down: their rows would carry the same (term, node) pairs.
    #[test]
    fn a_second_term_inside_one_slice_is_refused_naming_it() {
        // Two `<term>`s in ONE slice (not two slices sharing a term): the
        // count check cannot see it and `seen_terms` sees only the
        // survivor, so without this guard the slice published under the
        // second date with the first silently dropped.
        let doc = DOC.replacen(
            "<term>2026-09-18</term>",
            "<term>2026-09-18</term><term>2026-09-25</term>",
            1,
        );
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("term") && err.message.contains("already"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_repeated_slice_term_is_refused_naming_the_term() {
        let doc = DOC.replacen(
            "<slices>",
            "<slices><slice><term>2026-09-18</term><forward>1</forward><atm>1</atm><skew>1</skew><param>1</param><param>2</param><param>3</param></slice>",
            1,
        );
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(
            err.message.contains("2026-09-18") && err.message.contains("twice"),
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

    /// The path stack reports local names, so a namespaced document is
    /// the same document — and an unknown element deep inside a slice is
    /// still reported by its whole path, which is what makes the
    /// diagnostic say *where* the drift is rather than just that there is
    /// one.
    #[test]
    fn namespaces_are_ignored_and_a_deep_unknown_path_is_reported_whole() {
        let doc = DOC
            .replace("<marketData>", r#"<md:marketData xmlns:md="urn:desk">"#)
            .replace("</marketData>", "</md:marketData>")
            .replace(
                "<term>2026-10-16</term>",
                "<term>2026-10-16</term><tag>x</tag>",
            );
        let parsed = CviKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(
            parsed.unknown_paths,
            vec!["marketData/cviParams/slices/slice/tag".to_string()]
        );
    }

    /// The root is checked by name, so a document of some other shape
    /// fails saying so rather than reporting every one of its elements as
    /// unknown and then "underlying is missing".
    #[test]
    fn a_document_that_is_not_market_data_fails_naming_its_root() {
        let err = CviKind
            .parse(b"<vols><underlying>SPX.Z</underlying></vols>")
            .unwrap_err();
        assert!(
            err.message
                .contains("root element 'vols' is not 'marketData'"),
            "{}",
            err.message
        );
        let err = CviKind.parse(b"").unwrap_err();
        assert!(
            err.message.contains("no 'marketData' element"),
            "{}",
            err.message
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
        // A slice missing one of its per-slice values is refused naming
        // the slice's term and the tag — never filled from the previous
        // slice or left NULL (the long form has no NULL here: every node
        // row of the slice would have to carry one).
        for (needle, what) in [
            ("<forward>7671.25</forward>", "forward"),
            ("<atm>0.19</atm>", "atm"),
            ("<skew>-0.95</skew>", "skew"),
        ] {
            let doc = DOC.replace(needle, "");
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(
                err.message.contains("2026-10-16") && err.message.contains(what),
                "{what}: {}",
                err.message
            );
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
        <forward>7655.5</forward>
        <atm>0.182</atm>
        <skew>-1.1</skew>
        <param>-0.34</param>
        <param>0.1</param>
        <param>1.3</param>
      </slice>
      <slice>
        <term>2026-10-16</term>
        <forward>7671.25</forward>
        <atm>0.19</atm>
        <skew>-0.95</skew>
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

    /// A slice value is said once per `<slice>` on the wire, so rows of
    /// one slice that disagree on it have no honest surface: writing the
    /// first row's value would silently drop the other. Refused naming
    /// the term and the column — the ragged-slice rule's sibling.
    #[test]
    fn write_refuses_a_slice_whose_rows_disagree_on_a_slice_value() {
        for (i, column) in [(1, "forward"), (2, "atm"), (3, "skew")] {
            let mut rows = expected();
            let Column::F64(col) = &mut rows.values[i].1 else {
                panic!("slice values are f64 columns");
            };
            // The second slice's middle node disagrees with its first.
            col[4] += 0.5;
            let err = CviKind.write(&rows).unwrap_err();
            assert!(
                err.message.contains("2026-10-16"),
                "{column}: {}",
                err.message
            );
            assert!(err.message.contains(column), "{column}: {}", err.message);
            assert!(
                err.message.contains("within the slice"),
                "{column}: {}",
                err.message
            );
        }
        // Read off the FIRST row, not averaged: the surviving value in a
        // consistent slice is exactly the one every row carries.
        let text = String::from_utf8(CviKind.write(&expected()).unwrap()).unwrap();
        assert!(text.contains("<forward>7671.25</forward>"), "{text}");
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
                .contains("values are [param, forward, atm, skew]")
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
            let mut draw = || {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (x >> 11) as f64 / (1u64 << 53) as f64
            };
            let mut params = Vec::new();
            let mut slice_cols: [Vec<f64>; 3] = Default::default();
            let mut term_col = Vec::new();
            let mut node_col = Vec::new();
            for t in &term_dates {
                // One draw per slice value per term, repeated on every
                // node row — the shape the parser produces.
                let per_slice = [draw() * 9000.0, draw(), draw() * -2.0];
                for n in &nodes {
                    params.push(draw() * 4.0 - 2.0);
                    for (col, v) in slice_cols.iter_mut().zip(per_slice) {
                        col.push(v);
                    }
                    term_col.push(*t);
                    node_col.push(*n);
                }
            }
            let [forward, atm, skew] = slice_cols;
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
                values: vec![
                    ("param".into(), Column::F64(params)),
                    ("forward".into(), Column::F64(forward)),
                    ("atm".into(), Column::F64(atm)),
                    ("skew".into(), Column::F64(skew)),
                ],
            };
            let bytes = CviKind.write(&rows).unwrap();
            let parsed = CviKind.parse(&bytes).unwrap();
            proptest::prop_assert!(parsed.unknown_paths.is_empty());
            proptest::prop_assert_eq!(parsed.rows, rows);
        }
    }
}
