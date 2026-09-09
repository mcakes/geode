//! `Config::from_docs` — the merge half of the loader (spec §7.1).
//!
//! Phase 4c's config dialogs apply a field edit **instantly**: the edited
//! object is written into the in-memory user-layer `LayerDoc`, the whole
//! set is re-merged, and the result goes through the same
//! `hot_reload::apply_reload` the watcher uses. That puts this merge in
//! the keystroke path, where PHILOSOPHY's <8 ms pure-UI budget applies —
//! so it is measured rather than assumed.
//!
//! The fixture is the real demo desk (`examples/demo-config`, the largest
//! config this repo ships: a 7.8 KB `datasets.toml` and a 6.8 KB
//! `views.toml`) as the builtin layer, with a user-layer `views.toml` and
//! `view_presentation.toml` on top — the exact shape a trader editing a
//! view has.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{Config, Layer, LayerDoc};
use std::hint::black_box;

const APP: &str = include_str!("../../../examples/demo-config/app.toml");
const DATASETS: &str = include_str!("../../../examples/demo-config/datasets.toml");
const DIMENSIONS: &str = include_str!("../../../examples/demo-config/dimensions.toml");
const GROUPINGS: &str = include_str!("../../../examples/demo-config/groupings.toml");
const VIEWS: &str = include_str!("../../../examples/demo-config/views.toml");

fn docs() -> Vec<LayerDoc> {
    let mut out = vec![
        LayerDoc::builtin("app", APP).unwrap(),
        LayerDoc::builtin("datasets", DATASETS).unwrap(),
        LayerDoc::builtin("dimensions", DIMENSIONS).unwrap(),
        LayerDoc::builtin("groupings", GROUPINGS).unwrap(),
        LayerDoc::builtin("views", VIEWS).unwrap(),
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
