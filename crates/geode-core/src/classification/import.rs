//! Import planning: a CSV merged into one classification. Each row sets its
//! source's label, a blank label clears it, and sources absent from the file
//! keep their label. The header must name `<from>,<name>` exactly, so a file
//! exported from one classification cannot load into another by mistake.

use super::{Change, UndoEntry, csv};
use crate::dimensions::DerivedDimension;
use std::collections::BTreeMap;

/// Largest file an import reads. `geode-data` refuses a larger read.
pub const MAX_IMPORT_BYTES: u64 = 10 * 1024 * 1024;
/// Most data rows (header excluded) an import plans.
pub const MAX_IMPORT_ROWS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub line: usize,
    pub reason: String,
}

/// What an import would do, computed against the object at planning time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub changes: Vec<Change>,
    pub unchanged: usize,
    pub rejected: Vec<Rejected>,
}

impl ImportPlan {
    /// Rows that replace one label with another.
    pub fn changed(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.before.is_some() && c.after.is_some())
            .count()
    }
    /// Rows that label a previously unclassified source.
    pub fn added(&self) -> usize {
        self.changes.iter().filter(|c| c.before.is_none()).count()
    }
    /// Rows that clear a label.
    pub fn cleared(&self) -> usize {
        self.changes.iter().filter(|c| c.after.is_none()).count()
    }
    pub fn is_noop(&self) -> bool {
        self.changes.is_empty()
    }

    /// Apply the planned labels over `dim` (which may have moved on since
    /// planning: the file is authoritative for its rows) as one undo entry.
    pub fn apply(&self, dim: &DerivedDimension) -> (DerivedDimension, UndoEntry) {
        let mut next = dim.clone();
        let mut entry = UndoEntry::default();
        for change in &self.changes {
            let before = next.values.get(&change.source).cloned();
            if before == change.after {
                continue;
            }
            match &change.after {
                Some(l) => {
                    next.values.insert(change.source.clone(), l.clone());
                }
                None => {
                    next.values.remove(&change.source);
                }
            }
            entry.changes.push(Change {
                source: change.source.clone(),
                before,
                after: change.after.clone(),
            });
        }
        (next, entry)
    }
}

/// Plan merging CSV `text` into `dim`. `Err` refuses the whole file (empty,
/// unparseable, wrong header, too many rows); row problems are `rejected`.
pub fn plan_import(dim: &DerivedDimension, text: &str) -> Result<ImportPlan, String> {
    let records = csv::read(text).map_err(|e| e.to_string())?;
    let Some((header, rows)) = records.split_first() else {
        return Err("file is empty".into());
    };
    let got: Vec<&str> = header.fields.iter().map(|f| f.trim()).collect();
    if got != [dim.from.as_str(), dim.name.as_str()] {
        return Err(format!(
            "file is {}; this classification is {},{}",
            got.join(","),
            dim.from,
            dim.name
        ));
    }
    if rows.len() > MAX_IMPORT_ROWS {
        return Err(format!(
            "file has {} rows; an import takes at most {MAX_IMPORT_ROWS}",
            rows.len()
        ));
    }

    let mut plan = ImportPlan::default();
    // source -> (label, lines), in first-mention order; conflicting sources
    // are collected separately and rejected whole.
    let mut wanted: Vec<(String, Option<String>, Vec<usize>)> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut conflicted: BTreeMap<String, ()> = BTreeMap::new();
    for record in rows {
        if record.fields.len() != 2 {
            plan.rejected.push(Rejected {
                line: record.line,
                reason: format!("expected 2 fields, found {}", record.fields.len()),
            });
            continue;
        }
        let source = record.fields[0].trim().to_string();
        if source.is_empty() {
            plan.rejected.push(Rejected {
                line: record.line,
                reason: "empty source".into(),
            });
            continue;
        }
        let label = Some(record.fields[1].trim().to_string()).filter(|l| !l.is_empty());
        match index.get(&source) {
            Some(&i) => {
                if wanted[i].1 != label {
                    conflicted.insert(source.clone(), ());
                }
                wanted[i].2.push(record.line);
            }
            None => {
                index.insert(source.clone(), wanted.len());
                wanted.push((source, label, vec![record.line]));
            }
        }
    }
    for (source, label, lines) in wanted {
        if conflicted.contains_key(&source) {
            for line in lines {
                plan.rejected.push(Rejected {
                    line,
                    reason: format!("'{source}' is given two different labels"),
                });
            }
            continue;
        }
        let before = dim.values.get(&source).cloned();
        if before == label {
            plan.unchanged += 1;
        } else {
            plan.changes.push(Change {
                source,
                before,
                after: label,
            });
        }
    }
    plan.rejected.sort_by_key(|r| r.line);
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sector(pairs: &[(&str, &str)]) -> DerivedDimension {
        DerivedDimension {
            name: "sector".into(),
            from: "underlying_ref".into(),
            values: pairs
                .iter()
                .map(|(s, l)| (s.to_string(), l.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn a_header_for_another_classification_is_refused() {
        let err = plan_import(&sector(&[]), "underlying_ref,region\nAAPL,US\n").unwrap_err();
        assert_eq!(
            err,
            "file is underlying_ref,region; this classification is underlying_ref,sector"
        );
    }

    #[test]
    fn the_header_is_trimmed_but_case_sensitive() {
        assert!(plan_import(&sector(&[]), " underlying_ref , sector \n").is_ok());
        assert!(plan_import(&sector(&[]), "Underlying_Ref,sector\n").is_err());
    }

    #[test]
    fn an_empty_file_is_refused() {
        assert_eq!(plan_import(&sector(&[]), "").unwrap_err(), "file is empty");
    }

    #[test]
    fn rows_bucket_into_changed_new_cleared_and_unchanged() {
        let dim = sector(&[
            ("AAPL", "Tech"),
            ("XOM", "Energy"),
            ("CVX", "Energy"),
            ("KEEP", "X"),
        ]);
        let text = "underlying_ref,sector\nAAPL,Tech\nXOM,Utilities\nNKY,Index\nCVX,\n";
        let plan = plan_import(&dim, text).unwrap();
        assert_eq!((plan.changed(), plan.added(), plan.cleared()), (1, 1, 1));
        assert_eq!(plan.unchanged, 1);
        assert!(plan.rejected.is_empty());
        let (next, entry) = plan.apply(&dim);
        assert_eq!(
            next.values.get("XOM").map(String::as_str),
            Some("Utilities")
        );
        assert_eq!(next.values.get("NKY").map(String::as_str), Some("Index"));
        assert!(!next.values.contains_key("CVX"));
        assert_eq!(
            next.values.get("KEEP").map(String::as_str),
            Some("X"),
            "absent rows keep their label"
        );
        assert_eq!(
            entry.changes.len(),
            3,
            "one undo entry for the whole import"
        );
    }

    #[test]
    fn a_source_given_two_labels_rejects_both_rows_and_keeps_its_label() {
        let dim = sector(&[("AAPL", "Tech")]);
        let text = "underlying_ref,sector\nAAPL,Index\nXOM,Energy\nAAPL,Utilities\n";
        let plan = plan_import(&dim, text).unwrap();
        let lines: Vec<usize> = plan.rejected.iter().map(|r| r.line).collect();
        assert_eq!(lines, vec![2, 4]);
        assert!(
            plan.rejected[0].reason.contains("AAPL"),
            "{}",
            plan.rejected[0].reason
        );
        let (next, _) = plan.apply(&dim);
        assert_eq!(next.values.get("AAPL").map(String::as_str), Some("Tech"));
        assert_eq!(next.values.get("XOM").map(String::as_str), Some("Energy"));
    }

    #[test]
    fn a_source_repeated_with_the_same_label_counts_once() {
        let plan = plan_import(&sector(&[]), "underlying_ref,sector\nA,X\nA,X\n").unwrap();
        assert_eq!(plan.added(), 1);
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn malformed_rows_are_rejected_with_their_line() {
        let text = "underlying_ref,sector\nA\n,X\nB,Y,Z\nC,D\n";
        let plan = plan_import(&sector(&[]), text).unwrap();
        let got: Vec<(usize, &str)> = plan
            .rejected
            .iter()
            .map(|r| (r.line, r.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (2, "expected 2 fields, found 1"),
                (3, "empty source"),
                (4, "expected 2 fields, found 3"),
            ]
        );
        assert_eq!(plan.added(), 1);
    }

    #[test]
    fn nothing_to_change_is_a_noop_plan() {
        let plan = plan_import(&sector(&[("A", "X")]), "underlying_ref,sector\nA,X\n").unwrap();
        assert!(plan.is_noop());
    }

    #[test]
    fn too_many_rows_are_refused_before_planning() {
        let mut text = String::from("underlying_ref,sector\n");
        for i in 0..=MAX_IMPORT_ROWS {
            text.push_str(&format!("S{i},L\n"));
        }
        let err = plan_import(&sector(&[]), &text).unwrap_err();
        assert!(err.contains("100000"), "{err}");
    }

    #[test]
    fn a_parse_error_names_its_line() {
        let err = plan_import(&sector(&[]), "underlying_ref,sector\n\"open\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
    }
}
