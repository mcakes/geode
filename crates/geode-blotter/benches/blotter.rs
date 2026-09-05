//! Criterion benchmarks over the blotter's pure core (Phase 3 spec
//! §6, §7.4). Empty until Plan 3c Task 9 fills in the flatten/format
//! hot paths; kept here from the first commit so `cargo bench
//! --workspace --no-run` compiles cleanly (workspace invariant).
//!
//! `criterion_group!` requires at least one target (its macro pattern is
//! `$( $target:path ),+`, not `*`), so a truly empty group does not
//! compile at the pinned criterion version — `placeholder` is a no-op
//! kept only to satisfy that until Task 9 adds the real benches.

use criterion::{Criterion, criterion_group, criterion_main};

fn placeholder(_c: &mut Criterion) {}

criterion_group!(benches, placeholder);
criterion_main!(benches);
