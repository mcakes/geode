//! Criterion benchmarks over the shell's pure cores (spec §7.4): the
//! per-frame / per-keystroke logic whose budgets PHILOSOPHY.md treats as
//! contracts. Everything here is pure — no gpui, no window — exercised
//! through the same public APIs `ShellView` calls per frame (`Tree::layout`,
//! `divider_strips`, `resolve_drop_target`) or per keystroke
//! (`Matcher::press`, `PaletteState::set_query`), plus the session
//! serialization round-trip that runs on the ~500ms background flush.
//!
//! Regression gating in CI is deliberately deferred to Phase 2 (see
//! docs/perf.md); until then these run locally via
//! `cargo bench -p geode-shell` and compile-check in CI via
//! `cargo bench --workspace --no-run`.
//!
//! Sample sizes / measurement times are trimmed so the whole suite stays
//! in the tens of seconds — these are microbenchmarks of small pure
//! functions; criterion's defaults (100 samples, 3s+ per bench) buy no
//! extra signal at this scale.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

use geode_core::config::LayerDoc;
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
use geode_shell::keymap::{
    KeyContext, Keymap, MatchResult, Matcher, build_keymap, parse_keystroke,
};
use geode_shell::palette::{PaletteItem, PaletteState, fuzzy_match};
use geode_shell::session;
use geode_shell::shell::objectdialog::{Destination, Domain, apply};
use geode_shell::tiling::{
    DIVIDER_HIT_WIDTH, DockSide, Orientation, Rect, TileId, Tree, Workspaces, divider_strips,
    resolve_drop_target,
};

/// A 1440p-ish content area — the geometry ShellView hands `Tree::layout`.
const BOUNDS: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 2496.0,
    h: 1352.0,
};

/// A deep tree: every split alternates orientation at the (just-focused)
/// new tile, so each insert nests another split — depth grows with n.
fn deep_tree(n: u64) -> Tree {
    let mut tree = Tree::default();
    for i in 0..n {
        let orientation = if i % 2 == 0 {
            Orientation::Horizontal
        } else {
            Orientation::Vertical
        };
        tree.split(TileId(i), orientation);
    }
    tree
}

/// A wide tree: every split keeps the same orientation, so the inserts
/// flatten into one split node with n children.
fn wide_tree(n: u64) -> Tree {
    let mut tree = Tree::default();
    for i in 0..n {
        tree.split(TileId(i), Orientation::Horizontal);
    }
    tree
}

fn bench_tree_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_layout");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));
    for n in [10u64, 40, 100] {
        let deep = deep_tree(n);
        let wide = wide_tree(n);
        group.bench_function(format!("deep_{n}"), |b| {
            b.iter(|| black_box(deep.layout(black_box(BOUNDS))))
        });
        group.bench_function(format!("wide_{n}"), |b| {
            b.iter(|| black_box(wide.layout(black_box(BOUNDS))))
        });
    }
    group.finish();
}

fn bench_divider_strips(c: &mut Criterion) {
    let mut group = c.benchmark_group("divider_strips");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));
    for n in [10u64, 40, 100] {
        let deep = deep_tree(n);
        let wide = wide_tree(n);
        group.bench_function(format!("deep_{n}"), |b| {
            b.iter(|| {
                black_box(divider_strips(
                    black_box(&deep),
                    black_box(BOUNDS),
                    DIVIDER_HIT_WIDTH,
                ))
            })
        });
        group.bench_function(format!("wide_{n}"), |b| {
            b.iter(|| {
                black_box(divider_strips(
                    black_box(&wide),
                    black_box(BOUNDS),
                    DIVIDER_HIT_WIDTH,
                ))
            })
        });
    }
    group.finish();
}

fn bench_dropzones(c: &mut Criterion) {
    let mut group = c.benchmark_group("dropzones");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));

    // The per-mouse-move hit test a tile drag performs, swept over a
    // 16x9 grid of cursor positions across a 100-tile layout (each iter
    // = 144 resolves — a drag across the whole window).
    let tree = deep_tree(100);
    let tiles = tree.layout(BOUNDS);
    let sweep: Vec<(f32, f32)> = (0..16)
        .flat_map(|ix| {
            (0..9).map(move |iy| {
                (
                    BOUNDS.w * (ix as f32 + 0.5) / 16.0,
                    BOUNDS.h * (iy as f32 + 0.5) / 9.0,
                )
            })
        })
        .collect();
    group.bench_function("resolve_sweep_144pts_100tiles", |b| {
        b.iter(|| {
            for &(x, y) in &sweep {
                black_box(resolve_drop_target(
                    Vec::<(DockSide, Rect, &[(TileId, Rect)])>::new(),
                    black_box(&tiles),
                    x,
                    y,
                ));
            }
        })
    });
    group.finish();
}

/// The real builtin keymap, built exactly the way `ShellServices` builds it.
fn builtin_keymap() -> Keymap {
    let mut reg = ActionRegistry::default();
    register_builtin_actions(&mut reg);
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap parses");
    let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

fn bench_matcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher_press");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));

    let keymap = builtin_keymap();
    let stack = vec![KeyContext::new("workspace")];
    // Worst case: an unbound key scans every binding, matches none, and
    // finds no longer candidate either (NoMatch clears pending, so each
    // iter starts from the same empty state).
    let unbound = parse_keystroke("q", default_mod()).expect("parses");
    group.bench_function("no_match_full_keymap", |b| {
        let mut matcher = Matcher::default();
        b.iter(|| {
            let result = matcher.press(black_box(&keymap), black_box(unbound.clone()), &stack);
            assert!(matches!(result, MatchResult::NoMatch));
            black_box(result)
        })
    });
    group.finish();
}

/// Synthetic palette rows in the shape of real ones (`Action` rows with
/// title/category/binding, mixed with `Theme` rows) at count `n`.
fn palette_items(n: usize) -> Vec<PaletteItem> {
    let verbs = ["Focus", "Move", "Split", "Toggle", "Open", "Close"];
    let nouns = ["left", "right", "workspace", "dock", "palette", "theme"];
    (0..n)
        .map(|i| {
            if i % 8 == 7 {
                PaletteItem::Theme(format!("Synthetic Theme {i}"))
            } else {
                let verb = verbs[i % verbs.len()];
                let noun = nouns[(i / verbs.len()) % nouns.len()];
                PaletteItem::Action(
                    ActionId(format!("bench::action_{i}")),
                    format!("{verb} {noun} {i}"),
                    "Bench".to_string(),
                    (i % 3 == 0).then(|| "ctrl+x".to_string()),
                )
            }
        })
        .collect()
}

fn bench_palette(c: &mut Criterion) {
    let mut group = c.benchmark_group("palette");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));

    // One raw fuzzy match (the public lowercase-both wrapper), scored
    // against a realistic scattered-subsequence hit.
    group.bench_function("fuzzy_match_one", |b| {
        b.iter(|| {
            black_box(fuzzy_match(
                black_box("tgl splt"),
                black_box("Toggle split orientation"),
            ))
        })
    });

    // The full per-edit filter pass (`set_query` -> recompute_filtered:
    // match every item, sort, cache) at palette scale. Today's real size is
    // 84 (40 registry actions + 44 bundled themes); the 66 case below is
    // kept at its original value as a stable baseline rather than retuned
    // every time an action or theme is added, since what the bench measures
    // is the shape of the curve, not one exact count.
    // Queries alternate so consecutive iters never see identical state.
    for n in [66usize, 500, 2000] {
        let mut state = PaletteState::new(palette_items(n));
        group.bench_function(format!("set_query_{n}_items"), |b| {
            let mut flip = false;
            b.iter(|| {
                flip = !flip;
                state.set_query(if flip { "spl wk" } else { "tgl" });
                black_box(state.selected())
            })
        });
    }
    group.finish();
}

/// A realistic 9-workspace session: every workspace populated with 3–6
/// tiles in mixed orientations, a couple of docks in use — about what a
/// desk layout looks like after a week of muscle memory.
fn realistic_workspaces() -> Workspaces {
    let mut ws = Workspaces::new();
    for i in 1..=9u8 {
        ws.switch(i);
        let tiles = 3 + (i % 4) as usize; // 3..=6
        for t in 0..tiles {
            let orientation = if t % 2 == 0 {
                Orientation::Horizontal
            } else {
                Orientation::Vertical
            };
            ws.split_active(orientation);
        }
        if i % 3 == 0 {
            ws.active_mut().move_to_dock(DockSide::Left);
        }
        if i % 4 == 0 {
            ws.active_mut().move_to_dock(DockSide::Bottom);
        }
    }
    ws.switch(1);
    ws
}

fn bench_session(c: &mut Criterion) {
    let mut group = c.benchmark_group("session");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));

    let ws = realistic_workspaces();
    let no_tiles = session::TileRecords::new();
    group.bench_function("to_toml_9_workspaces", |b| {
        b.iter(|| black_box(session::to_toml(black_box(&ws), black_box(&no_tiles), None)))
    });

    let table = session::to_toml(&ws, &no_tiles, None);
    group.bench_function("from_toml_9_workspaces", |b| {
        b.iter(|| {
            let restored = session::from_toml(black_box(&table)).expect("round-trip parses");
            assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
            black_box(restored)
        })
    });
    group.finish();
}

/// What ONE KEYSTROKE in a config dialog costs, and what the debounced
/// flush behind it costs (Phase 4c §7.1).
///
/// The split matters and is the reason both are here. A keystroke
/// changes the draft, revalidates it, and renders the changed object —
/// all bounded by the one object being edited. It does **not** merge the
/// config or run `apply_reload`; those moved onto the 250 ms debounce
/// with the file write, because `apply_reload` emits `ConfigReloaded` and
/// every blotter tile requeries on it. `build_keymap` is the largest
/// thing `apply_reload` does unconditionally, so it is measured here as
/// the per-flush cost it now is rather than assumed free — it used to run
/// only on a 500 ms poll, and moving code from a poll into an interactive
/// path is exactly when "it was already there" stops being an excuse.
fn bench_config_edit(c: &mut Criterion) {
    let mut group = c.benchmark_group("config_edit");
    group.sample_size(50);
    group.measurement_time(Duration::from_millis(500));
    group.warm_up_time(Duration::from_millis(200));

    let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
        builtin: vec![
            LayerDoc::builtin("datasets", BENCH_DATASETS).unwrap(),
            LayerDoc::builtin("views", BENCH_VIEWS).unwrap(),
        ],
        desk: None,
        user: None,
    });

    // The keystroke's whole pure core: toggle the row, revalidate, render
    // the object for both destinations, and turn each into the value
    // memory and the file both take.
    group.bench_function("keystroke_toggle_validate_render", |b| {
        b.iter(|| {
            let mut draft = Domain::Views.draft(black_box(&config), "tree");
            draft.selected = 1;
            draft.toggle_selected();
            let diagnostics = Domain::Views.validate(&draft, &config);
            let mut out = Vec::new();
            for dest in [Destination::Doc, Destination::Presentation] {
                let table = Domain::Views.to_table(&draft, dest);
                out.push(apply::object_value(&draft.name, table));
            }
            black_box((diagnostics, out))
        })
    });

    // The flush's merge, over a builtin layer that includes the REAL
    // keymap — by far the largest document in the config model, and the
    // one `geode-core`'s own `config_merge` bench cannot reach (that
    // crate must not depend on this one). This is the honest per-flush
    // merge cost.
    let full_layer = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
        LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
        LayerDoc::builtin("views", DEMO_VIEWS).unwrap(),
    ];
    group.bench_function("flush_merge_full_layer", |b| {
        b.iter(|| {
            black_box(geode_core::config::Config::from_docs(black_box(
                full_layer.clone(),
            )))
        })
    });

    // What the debounced flush pays on top of that merge: `apply_reload`
    // rebuilds the keymap from the layered docs every time,
    // unconditionally.
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let keymap_docs = vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()];
    group.bench_function("flush_build_keymap", |b| {
        b.iter(|| {
            black_box(build_keymap(
                black_box(&keymap_docs),
                default_mod(),
                black_box(&registry),
            ))
        })
    });
    group.finish();
}

/// The demo desk's real `datasets`/`views` documents (7.8 KB and 6.8 KB —
/// the largest this repo ships), for the flush-merge bench above.
const DEMO_DATASETS: &str = include_str!("../../../examples/demo-config/datasets.toml");
const DEMO_VIEWS: &str = include_str!("../../../examples/demo-config/views.toml");

/// A desk-sized dataset and the view over it the config-edit bench edits.
const BENCH_DATASETS: &str = "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
     [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n\
     [risk_snapshot.columns.delta]\ntype = \"f64\"\nrole = \"measure\"\n\
     [risk_snapshot.columns.vega]\ntype = \"f64\"\nrole = \"measure\"\n";

const BENCH_VIEWS: &str = "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
     [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
     [[tree.columns]]\nname = \"npv\"\n\
     [[tree.columns]]\nname = \"delta\"\n\
     [[tree.columns]]\nname = \"vega\"\n";

criterion_group!(
    benches,
    bench_config_edit,
    bench_tree_layout,
    bench_divider_strips,
    bench_dropzones,
    bench_matcher,
    bench_palette,
    bench_session
);
criterion_main!(benches);
