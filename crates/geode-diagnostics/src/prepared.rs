//! Prepared tables: what a section paints, built from its typed rows with
//! expansion and filtering applied. Pure; `Rc`-shared with the delegate.

use std::collections::BTreeSet;
use std::ops::Range;
use std::time::SystemTime;

use geode_core::config::Severity;
use geode_core::log::Level;
use geode_core::query::ReferenceTable;
use geode_shell::listfilter::{ColumnMarks, Narrow};
use gpui::SharedString;

use crate::model::{
    ConfigDoc, DatasetRow, DiagnosticRow, Lane, PartitionRow, SourceRow, Tone, age_text,
    health_title,
};

/// Key and name are static for the fixed sections and built once per
/// prepared table for the Reference section, whose columns are the
/// dataset's declared names. The table asks for every header on every
/// frame, so a header hands out refcounted clones and never allocates.
#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub key: SharedString,
    pub name: SharedString,
    /// Width in pixels at the design rem (`shell::scale`).
    pub width: f32,
    pub right: bool,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub text: SharedString,
    pub tone: Tone,
    pub indent: u8,
    /// Byte ranges of `text` the filter matched, painted in the match
    /// accent; empty when no filter word landed in this cell.
    pub marks: Vec<Range<usize>>,
}

impl Cell {
    fn marked(mut self, marks: Vec<Range<usize>>) -> Cell {
        self.marks = marks;
        self
    }

    fn indented(mut self) -> Cell {
        self.indent = 1;
        self
    }
}

fn cell(text: impl Into<SharedString>, tone: Tone) -> Cell {
    Cell {
        text: text.into(),
        tone,
        indent: 0,
        marks: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Plain,
    Parent {
        expanded: bool,
    },
    Child,
    /// A full-width message row such as the log's loss report.
    Notice,
}

#[derive(Debug, Clone)]
pub struct PreparedRow {
    /// Stable identity: the source name, dataset name, `doc` or `doc.key`,
    /// or the log record's sequence.
    pub key: String,
    pub kind: RowKind,
    pub cells: Vec<Cell>,
    /// Lines for the detail strip when this row is the cursor.
    pub detail: Vec<SharedString>,
    pub tone: Tone,
}

#[derive(Debug, Clone, Default)]
pub struct PreparedTable {
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<PreparedRow>,
}

impl PreparedTable {
    pub fn empty() -> PreparedTable {
        PreparedTable::default()
    }

    /// The expandable key this row belongs to: itself for a parent, the
    /// nearest parent above for a child, none otherwise.
    pub fn parent_key_at(&self, ix: usize) -> Option<&str> {
        let row = self.rows.get(ix)?;
        match row.kind {
            RowKind::Parent { .. } => Some(row.key.as_str()),
            RowKind::Child => self.rows[..ix]
                .iter()
                .rev()
                .find(|r| matches!(r.kind, RowKind::Parent { .. }))
                .map(|r| r.key.as_str()),
            RowKind::Plain | RowKind::Notice => None,
        }
    }

    /// The cell to paint at `(row_ix, col_ix)`. A notice row has one cell
    /// and no colspan to span the row with, so it paints in the widest
    /// column, where it has room, and nothing anywhere else; every other
    /// row paints its cells in column order.
    pub fn cell_at(&self, row_ix: usize, col_ix: usize) -> Option<&Cell> {
        let row = self.rows.get(row_ix)?;
        match row.kind {
            RowKind::Notice if col_ix == widest_column(&self.columns) => row.cells.first(),
            RowKind::Notice => None,
            _ => row.cells.get(col_ix),
        }
    }
}

/// The index of the widest column; the first of equals. Zero for no
/// columns, which no table this crate builds has.
pub fn widest_column(columns: &[ColumnSpec]) -> usize {
    columns.iter().enumerate().fold(0, |best, (ix, c)| {
        if c.width > columns[best].width {
            ix
        } else {
            best
        }
    })
}

const fn col(key: &'static str, name: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec {
        key: SharedString::new_static(key),
        name: SharedString::new_static(name),
        width,
        right: false,
    }
}

const fn num(key: &'static str, name: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec {
        key: SharedString::new_static(key),
        name: SharedString::new_static(name),
        width,
        right: true,
    }
}

pub const SOURCE_COLUMNS: [ColumnSpec; 8] = [
    col("source", "Source", 160.0),
    col("health", "Health", 220.0),
    col("since", "Since", 130.0),
    col("shape", "Shape", 130.0),
    col("last_poll", "Last poll", 90.0),
    col("next_poll", "Next poll", 90.0),
    num("ready", "Ready", 60.0),
    col("loading", "Loading", 260.0),
];

/// The Since column's index in [`SOURCE_COLUMNS`]: the one cell the page's
/// ages tick rewrites in place.
pub const SINCE_COLUMN: usize = 2;

/// Between a Since cell's clock text and its age; the ages tick splits on
/// it to keep the clock text and replace the age.
pub const SINCE_SEPARATOR: &str = " · ";

/// The Sources table and, aligned with its filtered rows, each row's health
/// `since` time. The page's ages timer rewrites the Since cells from that
/// list in place, so it must index the rows the table paints, not every
/// typed row.
///
/// The filter narrows over every column. Since matches on its clock text
/// only: the age after it changes every second, and the ages tick keeps
/// the clock prefix, so marks placed there stay valid across ticks.
pub fn sources_table(
    rows: &[SourceRow],
    now: SystemTime,
    filter: &str,
) -> (PreparedTable, Vec<Option<SystemTime>>) {
    let mut narrow = Narrow::new(filter);
    let mut since_times = Vec::new();
    let rows = rows
        .iter()
        .filter_map(|r| {
            let marks = narrow.row(&[
                &r.name,
                &r.health,
                &r.since_hms,
                &r.shape,
                &r.last_poll,
                &r.next_poll,
                &r.ready,
                &r.loading,
            ])?;
            Some((r, marks))
        })
        .map(|(r, mut marks)| {
            since_times.push(r.since);
            let since = if r.since_hms.is_empty() {
                String::new()
            } else {
                format!("{}{SINCE_SEPARATOR}{}", r.since_hms, age_text(r.since, now))
            };
            let mut detail: Vec<SharedString> = Vec::with_capacity(r.detail.len() + 2);
            detail.push(format!("{} · {}", r.name, r.health).into());
            detail.extend(r.detail.iter().map(|s| SharedString::from(s.clone())));
            if !r.history.is_empty() {
                let history = r
                    .history
                    .iter()
                    .map(|(at, h)| format!("{} {at}", health_title(h)))
                    .collect::<Vec<_>>()
                    .join(" → ");
                detail.push(format!("history: {history}").into());
            }
            PreparedRow {
                key: r.name.clone(),
                kind: RowKind::Plain,
                cells: vec![
                    cell(r.name.clone(), Tone::Normal).marked(marks.take(0)),
                    cell(r.health.clone(), r.tone).marked(marks.take(1)),
                    cell(since, Tone::Muted).marked(marks.take(2)),
                    cell(r.shape.clone(), Tone::Muted).marked(marks.take(3)),
                    cell(r.last_poll.clone(), Tone::Muted).marked(marks.take(4)),
                    cell(r.next_poll.clone(), Tone::Muted).marked(marks.take(5)),
                    cell(r.ready.clone(), Tone::Muted).marked(marks.take(6)),
                    cell(r.loading.clone(), Tone::Muted).marked(marks.take(7)),
                ],
                detail,
                tone: r.tone,
            }
        })
        .collect();
    (
        PreparedTable {
            columns: SOURCE_COLUMNS.to_vec(),
            rows,
        },
        since_times,
    )
}

pub const DATA_COLUMNS: [ColumnSpec; 8] = [
    col("dataset", "Dataset", 220.0),
    num("partitions", "Partitions", 80.0),
    num("gen", "Latest gen", 90.0),
    col("published", "Published", 90.0),
    num("rows", "Rows", 110.0),
    num("resolved", "Resolved", 90.0),
    col("live", "Live", 70.0),
    col("loaded", "Loaded", 90.0),
];

/// A generation's searchable fields, in [`PARTITION_CELLS`] order.
fn partition_columns(row: &PartitionRow) -> [&str; 6] {
    [
        row.label.as_str(),
        row.gen_id.as_str(),
        row.source_time.as_str(),
        row.loaded.as_str(),
        row.rows.as_str(),
        row.kind,
    ]
}

/// The child-row cell each of [`partition_columns`] paints in: label,
/// generation, source time, loaded, rows, kind.
const PARTITION_CELLS: [usize; 6] = [0, 2, 3, 7, 4, 6];

pub fn data_table(
    rows: &[DatasetRow],
    collapsed: &BTreeSet<String>,
    filter: &str,
) -> PreparedTable {
    let mut narrow = Narrow::new(filter);
    let mut out = Vec::new();
    for r in rows {
        // A dataset matches on its name alone; a generation on its own
        // fields. Words do not combine across the two levels.
        let dataset_marks = narrow.row(&[&r.name]);
        let dataset_matches = dataset_marks.is_some();
        let children: Vec<(&PartitionRow, ColumnMarks)> = r
            .children
            .iter()
            .filter_map(|c| match narrow.row(&partition_columns(c)) {
                Some(marks) => Some((c, marks)),
                None if dataset_matches => Some((c, ColumnMarks::default())),
                None => None,
            })
            .collect();
        if !dataset_matches && children.is_empty() {
            continue;
        }
        // Search reveals matches without changing the saved expansion. A
        // matching dataset keeps all children; leaf matches keep their parent.
        let expanded = !narrow.is_empty() || !collapsed.contains(&r.name);
        let tone = if r.has_catalog {
            Tone::Normal
        } else {
            Tone::Muted
        };
        let name = if r.has_catalog {
            r.name.clone()
        } else {
            format!("{} (no catalog yet)", r.name)
        };
        out.push(PreparedRow {
            key: r.name.clone(),
            kind: RowKind::Parent { expanded },
            cells: vec![
                // `name` starts with the dataset name the marks index.
                cell(name, tone).marked(dataset_marks.map(|mut m| m.take(0)).unwrap_or_default()),
                cell(r.partitions.to_string(), Tone::Muted),
                cell(r.latest_gen.clone(), Tone::Normal),
                cell(r.published.clone(), Tone::Muted),
                cell(
                    format!("{} live · {} archive", r.live_rows, r.archive_rows),
                    Tone::Muted,
                ),
                cell(r.resolved.clone(), Tone::Marked),
                cell(String::new(), Tone::Muted),
                cell(String::new(), Tone::Muted),
            ],
            detail: vec![
                format!(
                    "{}: {} rows (est., live) · {} rows (est., archive)",
                    r.name, r.live_rows, r.archive_rows
                )
                .into(),
            ],
            tone,
        });
        if !expanded {
            continue;
        }
        for (c, mut marks) in children {
            let tone = if c.marked { Tone::Marked } else { Tone::Normal };
            let mut cells = vec![
                cell(c.label.clone(), Tone::Muted).indented(),
                cell(String::new(), Tone::Muted),
                cell(c.gen_id.clone(), tone),
                cell(c.source_time.clone(), tone),
                cell(c.rows.clone(), tone),
                cell(if c.marked { "●" } else { "" }, Tone::Marked),
                cell(c.kind, tone),
                cell(c.loaded.clone(), Tone::Muted),
            ];
            for (field, &cell_ix) in PARTITION_CELLS.iter().enumerate() {
                cells[cell_ix].marks = marks.take(field);
            }
            out.push(PreparedRow {
                key: format!("{}/{}/{}", r.name, c.label, c.gen_id),
                kind: RowKind::Child,
                cells,
                detail: vec![
                    format!(
                        "gen {} · source {} · loaded {} · rows {} · {}",
                        c.gen_id, c.source_time, c.loaded, c.rows, c.kind
                    )
                    .into(),
                ],
                tone,
            });
        }
    }
    PreparedTable {
        columns: DATA_COLUMNS.to_vec(),
        rows: out,
    }
}

pub const DIAGNOSTIC_COLUMNS: [ColumnSpec; 4] = [
    col("sev", "Sev", 70.0),
    col("lane", "Lane", 70.0),
    col("where", "Where", 240.0),
    col("message", "Message", 520.0),
];

pub const HISTORY_COLUMNS: [ColumnSpec; 5] = [
    col("batch", "Batch", 90.0),
    col("sev", "Sev", 70.0),
    col("lane", "Lane", 70.0),
    col("where", "Where", 240.0),
    col("message", "Message", 520.0),
];

/// The Current issues or History table, narrowed by `filter` over every
/// visible cell and the full diagnostic text the detail strip shows. Only
/// cells carry marks: a word that landed in the detail text keeps its row
/// without a highlight.
pub fn diagnostics_table(rows: &[DiagnosticRow], history: bool, filter: &str) -> PreparedTable {
    let mut narrow = Narrow::new(filter);
    let rows = rows
        .iter()
        .filter_map(|r| {
            let (sev, tone) = match r.severity {
                Severity::Error => ("error", Tone::Error),
                Severity::Warning => ("warn", Tone::Warn),
            };
            let lane = match r.lane {
                Lane::Config => "config",
                Lane::Data => "data",
            };
            let mut cells = Vec::with_capacity(5);
            if history {
                cells.push(cell(r.batch.clone().unwrap_or_default(), Tone::Muted));
            }
            cells.extend([
                cell(sev, tone),
                cell(lane, Tone::Muted),
                cell(r.location.clone(), Tone::Muted),
                cell(r.message.clone(), Tone::Normal),
            ]);
            if !narrow.is_empty() {
                let columns: Vec<&str> = cells
                    .iter()
                    .map(|c| c.text.as_ref())
                    .chain(std::iter::once(r.full.as_str()))
                    .collect();
                let mut marks = narrow.row(&columns)?;
                for (ix, c) in cells.iter_mut().enumerate() {
                    c.marks = marks.take(ix);
                }
            }
            Some(PreparedRow {
                key: format!("{}|{}", r.batch.clone().unwrap_or_default(), r.full),
                kind: RowKind::Plain,
                cells,
                detail: vec![r.full.clone().into()],
                tone,
            })
        })
        .collect();
    PreparedTable {
        columns: if history {
            HISTORY_COLUMNS.to_vec()
        } else {
            DIAGNOSTIC_COLUMNS.to_vec()
        },
        rows,
    }
}

pub const CONFIG_COLUMNS: [ColumnSpec; 3] = [
    col("key", "Key", 360.0),
    col("value", "Value", 360.0),
    col("layer", "Layer", 90.0),
];

pub fn config_table(docs: &[ConfigDoc], collapsed: &BTreeSet<String>) -> PreparedTable {
    let mut out = Vec::new();
    for doc in docs {
        let expanded = !collapsed.contains(&doc.name);
        let count = doc.leaves.len() + doc.omitted;
        out.push(PreparedRow {
            key: doc.name.clone(),
            kind: RowKind::Parent { expanded },
            cells: vec![
                cell(doc.name.clone(), Tone::Normal).marked(doc.name_marks.clone()),
                cell(format!("{count} leaves"), Tone::Muted),
                cell(String::new(), Tone::Muted),
            ],
            detail: vec![format!("{}: {count} leaves", doc.name).into()],
            tone: Tone::Normal,
        });
        if !expanded {
            continue;
        }
        for leaf in &doc.leaves {
            out.push(PreparedRow {
                key: format!("{}.{}", doc.name, leaf.key),
                kind: RowKind::Child,
                cells: vec![
                    cell(leaf.key.clone(), Tone::Normal)
                        .indented()
                        .marked(leaf.key_marks.clone()),
                    cell(leaf.value.clone(), Tone::Normal).marked(leaf.value_marks.clone()),
                    cell(leaf.layer.clone(), Tone::Muted),
                ],
                detail: vec![
                    format!(
                        "{}.{} = {}  [{}]",
                        doc.name, leaf.key, leaf.value, leaf.layer
                    )
                    .into(),
                ],
                tone: Tone::Normal,
            });
        }
        if doc.omitted > 0 {
            out.push(PreparedRow {
                key: format!("{}.…", doc.name),
                kind: RowKind::Child,
                cells: vec![
                    cell(format!("… {} more", doc.omitted), Tone::Muted).indented(),
                    cell(String::new(), Tone::Muted),
                    cell(String::new(), Tone::Muted),
                ],
                detail: Vec::new(),
                tone: Tone::Muted,
            });
        }
    }
    PreparedTable {
        columns: CONFIG_COLUMNS.to_vec(),
        rows: out,
    }
}

pub const LOG_COLUMNS: [ColumnSpec; 4] = [
    col("time", "Time", 110.0),
    col("level", "Lvl", 60.0),
    col("target", "Target", 170.0),
    col("message", "Message", 700.0),
];

fn level_tone(level: Level) -> Tone {
    match level {
        Level::ERROR => Tone::Error,
        Level::WARN => Tone::Warn,
        Level::INFO => Tone::Normal,
        _ => Tone::Muted,
    }
}

/// The loss notice that leads the Log table when the ring wrapped.
pub fn log_notice(lost: u64) -> PreparedRow {
    PreparedRow {
        key: "lost".into(),
        kind: RowKind::Notice,
        cells: vec![cell(
            format!("{lost} records lost — the ring wrapped before the last drain"),
            Tone::Warn,
        )],
        detail: Vec::new(),
        tone: Tone::Warn,
    }
}

/// One log record's row, unmarked: time, level, target, message.
pub fn log_row(
    seq: u64,
    level: Level,
    target: &'static str,
    hms_millis: SharedString,
    message: SharedString,
) -> PreparedRow {
    let tone = level_tone(level);
    let detail = format!("{hms_millis} {level} {target} {message}").into();
    PreparedRow {
        key: seq.to_string(),
        kind: RowKind::Plain,
        cells: vec![
            cell(hms_millis, Tone::Muted),
            cell(level.as_str(), tone),
            cell(target, Tone::Muted),
            cell(message, tone),
        ],
        detail: vec![detail],
        tone,
    }
}

/// A NULL reference cell; distinct from an empty string.
const NULL_TEXT: &str = "—";
/// Reference width per column at the design rem; the table resizes.
const REFERENCE_WIDTH: f32 = 120.0;

/// One row per reference row, columns as declared. The row key is the
/// first column's text, so selection follows a row through refreshes; that
/// identifies a row whenever the first key column is unique on its own,
/// which holds for every declared reference dataset today. A composite key
/// whose first column repeats would share selection between its rows. The
/// filter narrows fuzzily over every cell, as every section's does, and
/// marks the matched characters; a NULL cell's dash is never matched.
pub fn reference_table(table: Option<&ReferenceTable>, filter: &str) -> PreparedTable {
    let Some(table) = table else {
        return PreparedTable::empty();
    };
    let mut narrow = Narrow::new(filter);
    let text = |c: &Option<String>| c.as_deref().unwrap_or(NULL_TEXT).to_string();
    let columns = table
        .columns
        .iter()
        .map(|c| {
            let name = SharedString::from(c.clone());
            ColumnSpec {
                key: name.clone(),
                name,
                width: REFERENCE_WIDTH,
                right: false,
            }
        })
        .collect();
    let mut matched: Vec<&str> = Vec::with_capacity(table.columns.len());
    let rows = table
        .rows
        .iter()
        .filter_map(|r| {
            matched.clear();
            matched.extend(r.iter().map(|c| c.as_deref().unwrap_or("")));
            let mut marks = narrow.row(&matched)?;
            Some(PreparedRow {
                key: r.first().map(text).unwrap_or_default(),
                kind: RowKind::Plain,
                cells: r
                    .iter()
                    .enumerate()
                    .map(|(ix, c)| cell(text(c), Tone::Normal).marked(marks.take(ix)))
                    .collect(),
                detail: table
                    .columns
                    .iter()
                    .zip(r)
                    .map(|(name, c)| SharedString::from(format!("{name}: {}", text(c))))
                    .collect(),
                tone: Tone::Normal,
            })
        })
        .collect();
    PreparedTable { columns, rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::collections::BTreeSet;

    fn dataset(name: &str, children: usize) -> DatasetRow {
        DatasetRow {
            name: name.into(),
            has_catalog: true,
            live_rows: "1".into(),
            archive_rows: "0".into(),
            partitions: 1,
            latest_gen: "1".into(),
            published: "".into(),
            resolved: "".into(),
            children: (0..children)
                .map(|i| PartitionRow {
                    label: format!("p{i}"),
                    gen_id: i.to_string(),
                    source_time: "".into(),
                    loaded: "".into(),
                    rows: "".into(),
                    kind: "live",
                    marked: i == 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_collapsed_dataset_contributes_only_its_parent_row() {
        let rows = vec![dataset("risk", 2), dataset("vol", 1)];
        let open = data_table(&rows, &BTreeSet::new(), "");
        assert_eq!(open.rows.len(), 5);
        assert!(matches!(
            open.rows[0].kind,
            RowKind::Parent { expanded: true }
        ));
        assert!(matches!(open.rows[1].kind, RowKind::Child));
        assert_eq!(open.rows[1].tone, Tone::Marked, "resolved child is marked");
        let mut collapsed = BTreeSet::new();
        collapsed.insert("risk".to_string());
        let t = data_table(&rows, &collapsed, "");
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.parent_key_at(0), Some("risk"));
        assert_eq!(
            t.parent_key_at(2),
            Some("vol"),
            "a child resolves to its parent"
        );
        let filtered = data_table(&rows, &BTreeSet::new(), "vol");
        assert_eq!(filtered.rows.len(), 2);
    }

    #[test]
    fn data_filter_matches_leaf_fields_and_preserves_dataset_context() {
        let mut risk = dataset("risk", 2);
        risk.children[0] = PartitionRow {
            label: "2026-09-27 · EU_TECH".into(),
            gen_id: "654".into(),
            source_time: "08:12:34".into(),
            loaded: "09:23:45".into(),
            rows: "987".into(),
            kind: "archive",
            marked: true,
        };
        let rows = vec![risk, dataset("vol", 1)];
        let collapsed = BTreeSet::from(["risk".to_string()]);
        let keys = |table: &PreparedTable| {
            table
                .rows
                .iter()
                .map(|r| r.key.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        for query in [
            "2026-09-27",
            "eu_tech",
            "654",
            "08:12:34",
            "09:23:45",
            "987",
            "ARCHIVE",
        ] {
            let filtered = data_table(&rows, &collapsed, query);
            assert_eq!(
                keys(&filtered),
                ["risk", "risk/2026-09-27 · EU_TECH/654"],
                "{query}"
            );
            assert_eq!(filtered.rows[0].kind, RowKind::Parent { expanded: true });
            assert_eq!(filtered.parent_key_at(1), Some("risk"));
            assert_eq!(filtered.rows[1].tone, Tone::Marked);
            assert_eq!(
                filtered.rows[0].cells[4].text.as_ref(),
                "1 live · 0 archive",
                "filtering does not narrow catalog totals"
            );
        }
        let parent_match = data_table(&rows, &collapsed, "RISK");
        assert_eq!(
            parent_match.rows.len(),
            3,
            "a dataset match includes all its children"
        );
        assert!(data_table(&rows, &collapsed, "absent").rows.is_empty());
        let cleared = data_table(&rows, &collapsed, "");
        assert_eq!(keys(&cleared), ["risk", "vol", "vol/p0/0"]);
        assert_eq!(cleared.rows[0].kind, RowKind::Parent { expanded: false });
    }

    /// Prepared-table cost only: excludes the catalog model, GPUI, and paint.
    #[test]
    #[ignore]
    fn data_filter_timing_over_a_catalog() {
        let rows: Vec<_> = (0..20)
            .map(|i| dataset(&format!("dataset-{i}"), 200))
            .collect();
        let collapsed = BTreeSet::new();
        for query in ["", "dataset", "p100"] {
            let mut samples = Vec::new();
            let mut visible = 0;
            for _ in 0..20 {
                let start = std::time::Instant::now();
                let table = std::hint::black_box(data_table(&rows, &collapsed, query));
                visible = table.rows.len();
                samples.push(start.elapsed());
            }
            samples.sort();
            eprintln!(
                "data filter {query:?}, {visible} visible rows: median {:?}, max {:?} (20 datasets × 200 generations; 20 runs)",
                samples[10], samples[19]
            );
        }
    }

    /// The notice's one cell must land where it has room: the `Time`
    /// column would clip it to "7 records l…".
    #[test]
    fn a_notice_row_paints_its_cell_in_the_widest_column_only() {
        let message = LOG_COLUMNS.iter().position(|c| c.key == "message").unwrap();
        assert_eq!(widest_column(&LOG_COLUMNS), message);
        let t = PreparedTable {
            columns: LOG_COLUMNS.to_vec(),
            rows: vec![log_notice(7)],
        };
        assert!(t.cell_at(0, 0).is_none(), "nothing in Time");
        assert!(t.cell_at(0, 1).is_none());
        let cell = t.cell_at(0, message).expect("the notice in Message");
        assert!(cell.text.contains("7 records lost"));
        assert!(t.cell_at(1, message).is_none(), "no row there");
    }

    #[test]
    fn a_source_row_carries_the_age_and_its_detail_lines() {
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let rows = vec![SourceRow {
            name: "s".into(),
            tone: Tone::Normal,
            health: "Ok".into(),
            since: Some(now - std::time::Duration::from_secs(30)),
            since_hms: "00:01:10".into(),
            shape: "fetch".into(),
            last_poll: "".into(),
            next_poll: "".into(),
            ready: "".into(),
            loading: "".into(),
            detail: vec!["adapter: X".into(), "fetch".into()],
            history: vec![("00:00:01".into(), geode_shell::diagnostics::Health::Ok)],
        }];
        let (t, since_times) = sources_table(&rows, now, "");
        assert_eq!(SOURCE_COLUMNS[SINCE_COLUMN].key, "since");
        let since = &t.rows[0].cells[SINCE_COLUMN].text;
        assert_eq!(since.as_ref(), "00:01:10 · 30 s");
        assert_eq!(since_times, vec![rows[0].since], "aligned with the rows");
        assert_eq!(
            t.rows[0].detail.len(),
            4,
            "current health, spec lines, then history"
        );
        assert_eq!(t.rows[0].detail[0].as_ref(), "s · Ok");
        assert!(t.rows[0].detail[3].contains("Ok 00:00:01"));
        let (filtered, since_times) = sources_table(&rows, now, "zzz");
        assert!(filtered.rows.is_empty());
        assert!(since_times.is_empty(), "a filtered-out row has no since");
    }

    #[test]
    fn reference_rows_follow_the_declared_columns() {
        let t = reference_table(Some(&crate::model::tests::ref_table()), "");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_ref()).collect();
        assert_eq!(names, vec!["underlying_ref", "currency", "calendar"]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].key, "SPX");
        assert_eq!(
            t.rows[1].cells[1].text.as_ref(),
            "—",
            "NULL reads as a dash"
        );
        assert_eq!(t.rows[1].detail[1].as_ref(), "currency: —");
    }

    #[test]
    fn the_reference_filter_matches_any_cell_case_insensitively() {
        let t = reference_table(Some(&crate::model::tests::ref_table()), "xeur");
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].key, "SX5E");
        assert!(
            reference_table(Some(&crate::model::tests::ref_table()), "nothing")
                .rows
                .is_empty()
        );
    }

    /// The Reference filter is the shared fuzzy narrowing: words are
    /// subsequences within one cell, may land in different cells, and
    /// mark what they matched. A NULL cell's dash never matches.
    #[test]
    fn the_reference_filter_is_fuzzy_and_marks_cells() {
        let table = crate::model::tests::ref_table();
        let keys = |filter: &str| -> Vec<String> {
            reference_table(Some(&table), filter)
                .rows
                .into_iter()
                .map(|r| r.key)
                .collect()
        };
        assert_eq!(keys("xnys"), ["SPX"]);
        assert_eq!(keys("sx5 xer"), ["SX5E"], "words in two cells, fuzzy");
        assert!(keys("spx xeur").is_empty(), "every word must land");
        assert!(keys("—").is_empty(), "a NULL cell does not match its dash");
        let t = reference_table(Some(&table), "sx5 xer");
        let cells = &t.rows[0].cells;
        assert_eq!(marked(&cells[0]), ["SX5"]);
        assert!(cells[1].marks.is_empty(), "the NULL cell is unmarked");
        assert_eq!(marked(&cells[2]), ["XE", "R"]);
    }

    fn marked(cell: &Cell) -> Vec<&str> {
        cell.marks.iter().map(|r| &cell.text[r.clone()]).collect()
    }

    fn source(name: &str, health: &str, shape: &str, loading: &str) -> SourceRow {
        SourceRow {
            name: name.into(),
            tone: Tone::Normal,
            health: health.into(),
            since: None,
            since_hms: "09:15:00".into(),
            shape: shape.into(),
            last_poll: "".into(),
            next_poll: "".into(),
            ready: "3".into(),
            loading: loading.into(),
            detail: Vec::new(),
            history: Vec::new(),
        }
    }

    /// Sources narrow fuzzily, case-insensitively, over every column, and
    /// mark the cells the words landed in.
    #[test]
    fn the_sources_filter_is_fuzzy_over_every_column_and_marks_cells() {
        let now = std::time::SystemTime::UNIX_EPOCH;
        let rows = vec![
            source("Positions", "Ok", "fetch", ""),
            source(
                "vols",
                "Degraded — stale",
                "subscribe",
                "eu_tech 2026-09-27",
            ),
        ];
        let keys = |filter: &str| -> Vec<String> {
            sources_table(&rows, now, filter)
                .0
                .rows
                .into_iter()
                .map(|r| r.key)
                .collect()
        };
        assert_eq!(keys("positions"), ["Positions"], "name, any case");
        assert_eq!(keys("DGRD"), ["vols"], "health, fuzzy");
        assert_eq!(keys("subscr"), ["vols"], "shape");
        assert_eq!(keys("eutch"), ["vols"], "loading");
        assert_eq!(keys("09:15"), ["Positions", "vols"], "since clock text");
        assert_eq!(keys("pos ftch"), ["Positions"], "words in two columns");
        assert!(keys("pos subscribe").is_empty(), "every word must land");
        let (t, _) = sources_table(&rows, now, "vls stale");
        let cells = &t.rows[0].cells;
        assert_eq!(marked(&cells[0]), ["v", "ls"]);
        assert_eq!(marked(&cells[1]), ["stale"]);
        assert!(cells[2..].iter().all(|c| c.marks.is_empty()));
    }

    /// A match in the clock text of Since stays inside the prefix the
    /// ages tick keeps, so the marks survive the age being rewritten.
    #[test]
    fn since_marks_fall_in_the_clock_prefix_only() {
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let mut row = source("s", "Ok", "fetch", "");
        row.since = Some(now - std::time::Duration::from_secs(5));
        let (t, _) = sources_table(&[row], now, "15");
        let since = &t.rows[0].cells[SINCE_COLUMN];
        assert_eq!(since.text.as_ref(), "09:15:00 · 5 s");
        assert_eq!(marked(since), ["15"]);
        assert!(since.marks.iter().all(|r| r.end <= "09:15:00".len()));
        // No other column holds a `7`: the age "7 s" is all that could.
        let mut aged = source("x", "Ok", "fetch", "");
        aged.since = Some(now - std::time::Duration::from_secs(7));
        let (t, _) = sources_table(&[aged], now, "7");
        assert!(t.rows.is_empty(), "the ticking age is not matched");
    }

    /// Data keeps its two levels under a fuzzy filter: a dataset matches
    /// on its name and marks it; a generation on its own fields, marked in
    /// the cell each field paints in.
    #[test]
    fn the_data_filter_marks_dataset_names_and_generation_cells() {
        let mut risk = dataset("risk", 2);
        risk.children[0].gen_id = "654".into();
        risk.children[0].loaded = "09:23:45".into();
        risk.children[0].kind = "archive";
        risk.has_catalog = false;
        let rows = vec![risk, dataset("vol", 1)];
        let t = data_table(&rows, &BTreeSet::new(), "rsk");
        assert_eq!(t.rows.len(), 3, "a dataset match keeps every generation");
        assert_eq!(t.rows[0].cells[0].text.as_ref(), "risk (no catalog yet)");
        assert_eq!(marked(&t.rows[0].cells[0]), ["r", "sk"]);
        assert!(
            t.rows[1..]
                .iter()
                .all(|r| r.cells.iter().all(|c| c.marks.is_empty())),
            "unmatched generations kept by their dataset carry no marks"
        );
        let t = data_table(&rows, &BTreeSet::new(), "654 arch 23:45");
        let keys: Vec<_> = t.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["risk", "risk/p0/654"]);
        assert!(t.rows[0].cells[0].marks.is_empty(), "the parent is context");
        let leaf = &t.rows[1].cells;
        assert_eq!(marked(&leaf[2]), ["654"], "generation");
        assert_eq!(marked(&leaf[6]), ["arch"], "kind");
        assert_eq!(marked(&leaf[7]), ["23:45"], "loaded");
        assert!(
            data_table(&rows, &BTreeSet::new(), "risk 654")
                .rows
                .is_empty()
        );
    }

    #[test]
    fn no_reference_answer_is_an_empty_table() {
        assert!(reference_table(None, "").rows.is_empty());
    }

    fn issue(location: &str, message: &str, full: &str) -> DiagnosticRow {
        DiagnosticRow {
            severity: Severity::Warning,
            lane: Lane::Config,
            batch: Some("09:00:00".into()),
            location: location.into(),
            message: message.into(),
            full: full.into(),
        }
    }

    /// Issues narrow fuzzily over their cells and the full text the
    /// detail strip shows; only cells are marked.
    #[test]
    fn the_issue_filter_matches_cells_and_detail_and_marks_cells_only() {
        let rows = vec![
            issue(
                "keymap.toml › bindings",
                "unknown action",
                "keymap: unknown action x::y",
            ),
            issue(
                "app.toml › theme",
                "bad colour",
                "app: bad colour in theme.name",
            ),
        ];
        let keys = |t: &PreparedTable| {
            t.rows
                .iter()
                .map(|r| r.cells[2].text.to_string())
                .collect::<Vec<_>>()
        };
        let t = diagnostics_table(&rows, false, "unk act");
        assert_eq!(keys(&t), ["keymap.toml › bindings"]);
        assert_eq!(marked(&t.rows[0].cells[3]), ["unk", "act"]);
        let t = diagnostics_table(&rows, false, "x::y");
        assert_eq!(t.rows.len(), 1, "the detail text matches");
        assert!(
            t.rows[0].cells.iter().all(|c| c.marks.is_empty()),
            "but is not marked"
        );
        let t = diagnostics_table(&rows, true, "WARN thm");
        assert_eq!(t.rows.len(), 1);
        assert_eq!(
            marked(&t.rows[0].cells[1]),
            ["warn"],
            "the batch column shifts the cells"
        );
        assert_eq!(marked(&t.rows[0].cells[3]), ["th", "m"]);
        assert_eq!(diagnostics_table(&rows, true, " ").rows.len(), 2);
    }
}
