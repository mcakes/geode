//! Live reference tables shared with hosted modules.
//!
//! The shell installs an empty [`ReferenceGlobal`] at startup; the app's
//! bridge replaces it whenever a live read of a reference dataset changes a
//! table, and only then, so `cx.observe_global::<ReferenceGlobal>()` wakes a
//! module for a real change and never for a republish of the same rows. The
//! tables are always the live generation, never the frame's as-of. A failed
//! read keeps the last table rather than emptying it.

use std::sync::Arc;

use geode_core::reference::ReferenceData;

/// Every reference dataset's live table, shared by `Arc` so a clone for an
/// observer costs nothing. Written by the bridge only.
#[derive(Debug, Clone, Default)]
pub struct ReferenceGlobal(pub Arc<ReferenceData>);

impl gpui::Global for ReferenceGlobal {}
