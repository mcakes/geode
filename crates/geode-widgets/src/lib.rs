//! Shared widgets (as-of dialog spec 2026-09-20 §4): each widget is a
//! pure core (no `gpui` beyond `SharedString` for prepared text,
//! unit-testable) beside a painter that takes its colours as a value, so
//! the shell and any module paint the same thing through the same door.
//! Below the shell in the dependency graph — nothing here may name
//! `geode_shell` or a module.

pub mod datefield;
