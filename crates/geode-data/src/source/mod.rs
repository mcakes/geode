//! Sources: configured origins of data (spec §5.1). A source is an adapter
//! plus a list of directory globs, a refresh interval, a readiness
//! strategy, a priority, and a column map.

pub mod sentinel;

pub use sentinel::{Sentinel, SentinelError, parse_sentinel};
