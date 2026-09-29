//! Typed document parsers and writers. This crate is pure: it owns no I/O or
//! GPUI state. The app registers each kind with the data service, which sees
//! only `geode_core::document::DocumentKind`.

pub mod chain;
pub mod cvi;
pub mod dividend;

pub use chain::OptionChainKind;
pub use cvi::CviKind;
pub use dividend::DividendKind;

use geode_core::document::DocumentKind;
use std::sync::Arc;

/// Every kind this build knows, for the app to register.
/// `Arc<dyn DocumentKind>` rather than a static slice because the
/// registry hands the same kind to several sources at once and the
/// trait is the only thing `geode-data` sees.
pub fn builtin_kinds() -> Vec<Arc<dyn DocumentKind>> {
    vec![
        Arc::new(CviKind),
        Arc::new(DividendKind),
        Arc::new(OptionChainKind),
    ]
}
