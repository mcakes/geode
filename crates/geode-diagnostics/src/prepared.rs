//! Prepared tables: what a section paints, built from its typed rows with
//! expansion and filtering applied. Pure; `Rc`-shared with the delegate.

use std::collections::BTreeSet;
use std::time::SystemTime;

use geode_core::config::Severity;
use geode_core::log::Level;
use gpui::SharedString;

use crate::model::{
    ConfigDoc, DatasetRow, DiagnosticRow, Lane, LogRow, SourceRow, Tone, age_text, health_title,
};

#[derive(Debug, Clone, Copy)]
pub struct ColumnSpec {
    pub key: &'static str,
    pub name: &'static str,
    /// Width in pixels at the design rem (`shell::scale`).
    pub width: f32,
    pub right: bool,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub text: SharedString,
    pub tone: Tone,
    pub indent: u8,
}

fn cell(text: impl Into<SharedString>, tone: Tone) -> Cell {
    Cell {
        text: text.into(),
        tone,
        indent: 0,
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
        key,
        name,
        width,
        right: false,
    }
}

const fn num(key: &'static str, name: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec {
        key,
        name,
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
pub fn sources_table(
    rows: &[SourceRow],
    now: SystemTime,
    filter: &str,
) -> (PreparedTable, Vec<Option<SystemTime>>) {
    let mut since_times = Vec::new();
    let rows = rows
        .iter()
        .filter(|r| filter.is_empty() || r.name.contains(filter) || r.health.contains(filter))
        .map(|r| {
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
                    cell(r.name.clone(), Tone::Normal),
                    cell(r.health.clone(), r.tone),
                    cell(since, Tone::Muted),
                    cell(r.shape.clone(), Tone::Muted),
                    cell(r.last_poll.clone(), Tone::Muted),
                    cell(r.next_poll.clone(), Tone::Muted),
                    cell(r.ready.clone(), Tone::Muted),
                    cell(r.loading.clone(), Tone::Muted),
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

pub fn data_table(
    rows: &[DatasetRow],
    collapsed: &BTreeSet<String>,
    filter: &str,
) -> PreparedTable {
    let mut out = Vec::new();
    for r in rows
        .iter()
        .filter(|r| filter.is_empty() || r.name.contains(filter))
    {
        let expanded = !collapsed.contains(&r.name);
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
                cell(name, tone),
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
        for c in &r.children {
            let tone = if c.marked { Tone::Marked } else { Tone::Normal };
            out.push(PreparedRow {
                key: format!("{}/{}/{}", r.name, c.label, c.gen_id),
                kind: RowKind::Child,
                cells: vec![
                    Cell {
                        text: c.label.clone().into(),
                        tone: Tone::Muted,
                        indent: 1,
                    },
                    cell(String::new(), Tone::Muted),
                    cell(c.gen_id.clone(), tone),
                    cell(c.source_time.clone(), tone),
                    cell(c.rows.clone(), tone),
                    cell(if c.marked { "●" } else { "" }, Tone::Marked),
                    cell(c.kind, tone),
                    cell(c.loaded.clone(), Tone::Muted),
                ],
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

pub fn diagnostics_table(rows: &[DiagnosticRow], history: bool) -> PreparedTable {
    let rows = rows
        .iter()
        .map(|r| {
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
            PreparedRow {
                key: format!("{}|{}", r.batch.clone().unwrap_or_default(), r.full),
                kind: RowKind::Plain,
                cells,
                detail: vec![r.full.clone().into()],
                tone,
            }
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
                cell(doc.name.clone(), Tone::Normal),
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
                    Cell {
                        text: leaf.key.clone().into(),
                        tone: Tone::Normal,
                        indent: 1,
                    },
                    cell(leaf.value.clone(), Tone::Normal),
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
                    Cell {
                        text: format!("… {} more", doc.omitted).into(),
                        tone: Tone::Muted,
                        indent: 1,
                    },
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

pub fn log_table(rows: &[LogRow], lost: u64) -> PreparedTable {
    let mut out = Vec::with_capacity(rows.len() + 1);
    if lost > 0 {
        out.push(PreparedRow {
            key: "lost".into(),
            kind: RowKind::Notice,
            cells: vec![cell(
                format!("{lost} records lost — the ring wrapped before the last drain"),
                Tone::Warn,
            )],
            detail: Vec::new(),
            tone: Tone::Warn,
        });
    }
    out.extend(rows.iter().map(|r| {
        let tone = level_tone(r.level);
        PreparedRow {
            key: r.seq.to_string(),
            kind: RowKind::Plain,
            cells: vec![
                cell(r.hms_millis.clone(), Tone::Muted),
                cell(r.level.to_string(), tone),
                cell(r.target, Tone::Muted),
                cell(r.message.clone(), tone),
            ],
            detail: vec![format!("{} {} {} {}", r.hms_millis, r.level, r.target, r.message).into()],
            tone,
        }
    }));
    PreparedTable {
        columns: LOG_COLUMNS.to_vec(),
        rows: out,
    }
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
    fn the_log_table_leads_with_a_loss_notice_when_records_were_lost() {
        let rows = vec![LogRow {
            hms_millis: "09:00:00.000".into(),
            level: geode_core::log::Level::ERROR,
            target: "geode::shell",
            message: "boom".into(),
            seq: 1,
        }];
        let t = log_table(&rows, 7);
        assert!(matches!(t.rows[0].kind, RowKind::Notice));
        assert!(t.rows[0].cells[0].text.contains("7 records lost"));
        assert_eq!(t.rows[1].tone, Tone::Error);
        assert_eq!(
            t.rows[1].detail,
            vec![gpui::SharedString::from(
                "09:00:00.000 ERROR geode::shell boom"
            )]
        );
        assert_eq!(log_table(&rows, 0).rows.len(), 1);
    }

    /// The notice's one cell must land where it has room: the `Time`
    /// column would clip it to "7 records l…".
    #[test]
    fn a_notice_row_paints_its_cell_in_the_widest_column_only() {
        let message = LOG_COLUMNS.iter().position(|c| c.key == "message").unwrap();
        assert_eq!(widest_column(&LOG_COLUMNS), message);
        let t = log_table(&[], 7);
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
}
