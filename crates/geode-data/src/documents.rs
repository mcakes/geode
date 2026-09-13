//! The registry of `DocumentKind` parsers/writers `geode-app` fills at
//! startup, one per format a source can declare (market-data spec §6.4).
//! `geode-data` depends on `geode-core` alone here — never on
//! `geode-documents` or any other parser crate — because `geode-app` is
//! the one place that both wires sources to kinds and needs the concrete
//! parser implementations; this crate sees only the trait.

use geode_core::document::DocumentKind;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct DocumentRegistry {
    kinds: HashMap<String, Arc<dyn DocumentKind>>,
}

impl DocumentRegistry {
    /// Registers a kind, keyed by `kind.name()`. A second registration
    /// under the same name replaces the first rather than being refused
    /// — there is no ordering guarantee across `geode-app`'s startup
    /// wiring that would make "first wins" the safer default — but it is
    /// logged, since which kind answers a given name from then on would
    /// otherwise be silent.
    pub fn register(&mut self, kind: Arc<dyn DocumentKind>) {
        let name = kind.name().to_string();
        if self.kinds.insert(name.clone(), kind).is_some() {
            tracing::warn!(
                target: "geode::ingest",
                "document kind '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn DocumentKind>> {
        self.kinds.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.kinds.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::document::{DocumentRows, ParseError, ParsedDocument, WriteError};
    use geode_core::schema::ColumnType;

    struct TestKind {
        name: &'static str,
        columns: Vec<(&'static str, ColumnType)>,
    }

    impl DocumentKind for TestKind {
        fn name(&self) -> &'static str {
            self.name
        }
        fn columns(&self) -> &[(&'static str, ColumnType)] {
            &self.columns
        }
        fn parse(&self, _bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            Err(ParseError {
                message: "TestKind does not parse".into(),
            })
        }
        fn write(&self, _rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            Err(WriteError {
                message: "TestKind does not write".into(),
            })
        }
    }

    #[test]
    fn register_get_names_sorted_and_a_replacement_wins() {
        let mut reg = DocumentRegistry::default();
        let cvi: Arc<dyn DocumentKind> = Arc::new(TestKind {
            name: "cvi_xml",
            columns: Vec::new(),
        });
        let vol: Arc<dyn DocumentKind> = Arc::new(TestKind {
            name: "vol_csv",
            columns: Vec::new(),
        });
        reg.register(cvi.clone());
        reg.register(vol.clone());

        assert!(Arc::ptr_eq(&reg.get("cvi_xml").unwrap(), &cvi));
        assert!(Arc::ptr_eq(&reg.get("vol_csv").unwrap(), &vol));
        assert!(reg.get("nonesuch").is_none());
        assert_eq!(
            reg.names(),
            vec!["cvi_xml".to_string(), "vol_csv".to_string()]
        );

        let cvi2: Arc<dyn DocumentKind> = Arc::new(TestKind {
            name: "cvi_xml",
            columns: Vec::new(),
        });
        reg.register(cvi2.clone());
        assert!(Arc::ptr_eq(&reg.get("cvi_xml").unwrap(), &cvi2));
        assert!(!Arc::ptr_eq(&reg.get("cvi_xml").unwrap(), &cvi));
        assert_eq!(
            reg.names(),
            vec!["cvi_xml".to_string(), "vol_csv".to_string()]
        );
    }
}
