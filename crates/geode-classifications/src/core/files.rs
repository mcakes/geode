//! The tile's file operations: what an export or import asked for, kept
//! until its answer arrives, and what the notices say about the file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use geode_core::classification::import::{ImportPlan, Rejected};
use geode_core::dimensions::DerivedDimension;

/// A file read or write the tile asked the data tier for. Only the latest
/// is answered: an answer carrying another tag was overtaken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Write classification `name` to `path`, `rows` data rows.
    Export {
        tag: u64,
        name: String,
        path: PathBuf,
        rows: usize,
    },
    /// Read `path` into classification `name`.
    Import {
        tag: u64,
        name: String,
        path: PathBuf,
    },
}

impl FileOp {
    pub fn tag(&self) -> u64 {
        match self {
            FileOp::Export { tag, .. } | FileOp::Import { tag, .. } => *tag,
        }
    }
}

/// The file as a notice names it: its file name, else the whole path (a
/// root or a path ending in `..` has none).
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// How many data rows `classification::export` writes for the same
/// arguments: every mapped source, and with `observed` every observed one
/// too, each once.
pub fn export_rows(dim: &DerivedDimension, observed: Option<&[(String, u64)]>) -> usize {
    let mut sources: BTreeSet<&str> = dim.values.keys().map(String::as_str).collect();
    for (source, _) in observed.unwrap_or_default() {
        sources.insert(source);
    }
    sources.len()
}

/// `exported 3 rows to region.csv`.
pub fn exported(rows: usize, path: &Path) -> String {
    let noun = if rows == 1 { "row" } else { "rows" };
    format!("exported {rows} {noun} to {}", file_name(path))
}

/// The most rejected rows the notice lists; the rest are counted.
pub const REJECTED_LISTED: usize = 20;

/// The confirm's question: `import region.csv: 2 changed, 1 new, 1
/// cleared, 1 rejected — y applies`, the rejected count left out when
/// there are none.
pub fn import_question(file: &str, plan: &ImportPlan) -> String {
    let rejected = match plan.rejected.len() {
        0 => String::new(),
        n => format!(", {n} rejected"),
    };
    format!(
        "import {file}: {} changed, {} new, {} cleared{rejected} \u{2014} y applies",
        plan.changed(),
        plan.added(),
        plan.cleared()
    )
}

/// `import: nothing to change`, with the rejected count when any.
pub fn nothing_to_change(rejected: usize) -> String {
    match rejected {
        0 => "import: nothing to change".into(),
        n => format!("import: nothing to change ({n} rejected)"),
    }
}

/// The rejected rows on one line: the first [`REJECTED_LISTED`], then how
/// many more. Bounded, so a file of bad rows cannot build a notice the
/// size of the file; the notice renderer cuts what the width cannot hold.
pub fn rejected_notice(rejected: &[Rejected]) -> String {
    let listed: Vec<String> = rejected
        .iter()
        .take(REJECTED_LISTED)
        .map(|r| format!("line {}: {}", r.line, r.reason))
        .collect();
    let mut text = format!("rejected: {}", listed.join("; "));
    let more = rejected.len().saturating_sub(REJECTED_LISTED);
    if more > 0 {
        text.push_str(&format!("; and {more} more"));
    }
    text
}

/// `imported 4 rows into region`.
pub fn imported(rows: usize, name: &str) -> String {
    let noun = if rows == 1 { "row" } else { "rows" };
    format!("imported {rows} {noun} into {name}")
}

/// What an import answered after another classification was shown says.
pub fn not_applied(file: &str, name: &str) -> String {
    format!("import of {file} was for {name} \u{2014} not applied")
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::classification;

    fn region(pairs: &[(&str, &str)]) -> DerivedDimension {
        DerivedDimension {
            name: "region".into(),
            from: "underlying_ref".into(),
            values: pairs
                .iter()
                .map(|(s, l)| (s.to_string(), l.to_string()))
                .collect(),
        }
    }

    #[test]
    fn the_file_name_else_the_whole_path() {
        assert_eq!(file_name(Path::new("/tmp/out/region.csv")), "region.csv");
        assert_eq!(file_name(Path::new("/")), "/");
    }

    #[test]
    fn the_tag_of_either_op() {
        let path = PathBuf::from("x.csv");
        let e = FileOp::Export {
            tag: 3,
            name: "region".into(),
            path: path.clone(),
            rows: 0,
        };
        let i = FileOp::Import {
            tag: 4,
            name: "region".into(),
            path,
        };
        assert_eq!((e.tag(), i.tag()), (3, 4));
    }

    /// The count is the export's own data rows: an observed source already
    /// mapped is one row, not two.
    #[test]
    fn export_rows_counts_what_export_writes() {
        let dim = region(&[("DAX", "Europe"), ("SPX", "Americas")]);
        let observed = vec![("DAX".to_string(), 5), ("NKY".to_string(), 7)];
        for observed in [None, Some(observed.as_slice())] {
            let text = classification::export(&dim, observed);
            assert_eq!(
                export_rows(&dim, observed),
                text.lines().count() - 1,
                "{text}"
            );
        }
        assert_eq!(export_rows(&dim, Some(&observed)), 3);
    }

    #[test]
    fn one_row_is_singular() {
        let p = Path::new("/tmp/region.csv");
        assert_eq!(exported(1, p), "exported 1 row to region.csv");
        assert_eq!(exported(8, p), "exported 8 rows to region.csv");
    }

    fn rejected(n: usize) -> Vec<Rejected> {
        (0..n)
            .map(|i| Rejected {
                line: i + 2,
                reason: "empty source".into(),
            })
            .collect()
    }

    #[test]
    fn the_question_counts_each_kind_and_omits_no_rejections() {
        let dim = region(&[("SPX", "Americas"), ("DAX", "Europe")]);
        let plan = classification::import::plan_import(
            &dim,
            "underlying_ref,region\nSPX,Asia\nNKY,Asia\nDAX,\n",
        )
        .unwrap();
        assert_eq!(
            import_question("r.csv", &plan),
            "import r.csv: 1 changed, 1 new, 1 cleared \u{2014} y applies"
        );
        let plan = ImportPlan {
            rejected: rejected(2),
            ..plan
        };
        assert_eq!(
            import_question("r.csv", &plan),
            "import r.csv: 1 changed, 1 new, 1 cleared, 2 rejected \u{2014} y applies"
        );
    }

    #[test]
    fn nothing_to_change_counts_rejections_only_when_any() {
        assert_eq!(nothing_to_change(0), "import: nothing to change");
        assert_eq!(
            nothing_to_change(2),
            "import: nothing to change (2 rejected)"
        );
    }

    /// Twenty rows are listed whole; past that the rest are counted, so a
    /// file of bad rows builds a bounded notice.
    #[test]
    fn the_rejected_notice_lists_twenty_then_counts() {
        let twenty = rejected_notice(&rejected(REJECTED_LISTED));
        assert_eq!(twenty.matches("line ").count(), 20);
        assert!(!twenty.contains("more"), "{twenty}");
        assert!(twenty.starts_with("rejected: line 2: empty source; line 3:"));
        let many = rejected_notice(&rejected(25));
        assert_eq!(many.matches("line ").count(), 20);
        assert!(
            many.ends_with("line 21: empty source; and 5 more"),
            "{many}"
        );
    }

    #[test]
    fn imported_and_not_applied_name_the_file_and_classification() {
        assert_eq!(imported(1, "region"), "imported 1 row into region");
        assert_eq!(imported(4, "region"), "imported 4 rows into region");
        assert_eq!(
            not_applied("r.csv", "region"),
            "import of r.csv was for region \u{2014} not applied"
        );
    }
}
