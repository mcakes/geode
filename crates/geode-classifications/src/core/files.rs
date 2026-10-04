//! The tile's file operations: what an export or import asked for, kept
//! until its answer arrives, and what the notices say about the file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
}
