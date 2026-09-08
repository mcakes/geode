//! Re-export of [`geode_core::health::Health`] (Phase 4b Task 4): the
//! type moved to `geode-core` so `geode-shell`'s `Diagnostics` entity can
//! name it without `geode-shell` depending on `geode-data` (CLAUDE.md:
//! shell and data never depend on each other). Kept under this path so
//! every existing `geode_data::health::Health` / `crate::health::Health`
//! reference in this crate and its callers keeps compiling unchanged.
pub use geode_core::health::Health;
