//! Document kinds (market-data spec §6): one typed parser and writer per
//! kind, pure — no I/O, no gpui — depended on by the app (which registers
//! them into the data service) and by nothing in `geode-data` itself: the
//! service sees only `geode_core::document::DocumentKind`. Hand-written
//! over `quick-xml` now; regenerated from the desk's XSDs behind the same
//! two functions later (roadmap ruling 8).

pub mod cvi;

pub use cvi::CviKind;

use geode_core::document::DocumentKind;
use std::sync::Arc;

/// Every kind this build knows, for the app to register (spec §6.4).
/// `Arc<dyn DocumentKind>` rather than a static slice because the
/// registry hands the same kind to several sources at once and the
/// trait is the only thing `geode-data` sees.
pub fn builtin_kinds() -> Vec<Arc<dyn DocumentKind>> {
    vec![Arc::new(CviKind)]
}
