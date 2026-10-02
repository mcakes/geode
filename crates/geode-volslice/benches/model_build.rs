//! Slice-model preparation. The group stays empty until the model exists;
//! the target is declared from the start so the bench builds with the crate.

use criterion::{Criterion, criterion_group, criterion_main};

fn model_build(c: &mut Criterion) {
    let _group = c.benchmark_group("volslice_model_build");
}

criterion_group!(benches, model_build);
criterion_main!(benches);
