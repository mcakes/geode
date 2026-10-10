//! Pivot build bench: a 40 × 40 matrix.
use criterion::{Criterion, criterion_group, criterion_main};
fn build(_c: &mut Criterion) {}
criterion_group!(benches, build);
criterion_main!(benches);
