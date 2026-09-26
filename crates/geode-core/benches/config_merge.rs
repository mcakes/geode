//! Measure in-memory configuration merging through `Config::from_docs`.
//! Configuration dialog edits update the user-layer documents, re-merge them,
//! and apply the result through hot reload, so merge cost contributes to UI
//! response latency.
//!
//! The fixture layers demo configuration with user view and presentation edits.
//! It excludes the shell's builtin keymap and measures only this subset; the
//! shell_cores benchmark covers merging the full application layer.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{Config, Layer, LayerDoc};
use std::hint::black_box;

const APP: &str = include_str!("../../../examples/demo-config/app.toml");
const DATASETS: &str = include_str!("../../../examples/demo-config/datasets.toml");
const DIMENSIONS: &str = include_str!("../../../examples/demo-config/dimensions.toml");
const GROUPINGS: &str = include_str!("../../../examples/demo-config/groupings.toml");
const VIEWS: &str = include_str!("../../../examples/demo-config/views.toml");
const SCOPES: &str = "[eu_books]\nbook = [\"EU_RATES\", \"EU_CREDIT\"]\n";

fn docs() -> Vec<LayerDoc> {
    let mut out = vec![
        LayerDoc::builtin("app", APP).unwrap(),
        LayerDoc::builtin("datasets", DATASETS).unwrap(),
        LayerDoc::builtin("dimensions", DIMENSIONS).unwrap(),
        LayerDoc::builtin("groupings", GROUPINGS).unwrap(),
        LayerDoc::builtin("views", VIEWS).unwrap(),
        // A saved scope supplements the demo documents. The builtin keymap lives
        // in geode-shell, which cannot be a dependency of geode-core; its cost is
        // covered by shell_cores/config_edit/flush_merge_full_layer.
        LayerDoc::builtin("scopes", SCOPES).unwrap(),
    ];
    // The two user-layer docs a Views edit actually writes.
    for (name, text) in [
        (
            "views",
            "config_version = 1\n[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"npv\"\n",
        ),
        (
            "view_presentation",
            "config_version = 1\n[tree]\nhidden = [\"book\"]\n[tree.width]\nnpv = 120.0\n",
        ),
    ] {
        out.push(LayerDoc {
            layer: Layer::User,
            name: name.to_string(),
            file: format!("/tmp/{name}.toml").into(),
            table: text.parse().unwrap(),
        });
    }
    out
}

fn bench(c: &mut Criterion) {
    let docs = docs();
    c.bench_function("config_from_docs_demo_desk", |b| {
        b.iter(|| black_box(Config::from_docs(black_box(docs.clone()))))
    });
    // What one keystroke actually pays: clone the documents out of the
    // live `Config`, and merge them again.
    let config = Config::from_docs(docs);
    c.bench_function("config_all_docs_then_from_docs", |b| {
        b.iter(|| black_box(Config::from_docs(black_box(&config).all_docs())))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
