//! Layered placement of a pipeline's stages, pure.
//!
//! A stage's layer is the longest path from a source (sources are layer
//! 0). Within a layer, stages order by the barycentre of their inputs'
//! rows, then by that of their consumers' rows (two sweeps); equal keys
//! fall back to document order, so a pipeline with no crossings to resolve
//! reads as its document does. Cards are a fixed size at the design rem.
//! Columns spread evenly across the width down to a minimum gap; past
//! that the gaps hold the minimum and `content_w` exceeds the viewport,
//! which is the tile's cue to scroll. Rows do the same against the height
//! under a caption band. The layout is a function of (pipeline, size,
//! rem) computed when one of those changes, never per frame.

use geode_core::calibration::{Pipeline, StageKind, StageName};

/// Card width, in rem.
pub const CARD_W_REM: f32 = 9.0;
/// Card height, in rem.
pub const CARD_H_REM: f32 = 3.6;
/// The gap between columns and between rows below which the graph scrolls
/// rather than packing cards closer, in rem.
pub const MIN_GAP_REM: f32 = 2.0;
/// The band above the first row that holds the column captions, in rem.
pub const CAPTION_H_REM: f32 = 1.2;

pub struct LayoutInput<'a> {
    pub pipeline: &'a Pipeline,
    /// The viewport, in design pixels.
    pub width: f32,
    pub height: f32,
    pub rem: f32,
}

/// One stage's place: its layer and row, and its top-left corner in
/// design pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub stage: StageName,
    pub layer: usize,
    pub row: usize,
    pub x: f32,
    pub y: f32,
}

/// The placed graph. `cards` is in layout order: layer-major, then row.
/// `content_w` and `content_h` are at least the viewport; more means the
/// graph scrolls on that axis. `captions` pairs each layer with its text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub cards: Vec<Card>,
    pub layers: usize,
    pub card_w: f32,
    pub card_h: f32,
    pub content_w: f32,
    pub content_h: f32,
    pub captions: Vec<(usize, String)>,
}

/// A cursor step over the graph. The tile maps keys onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
}

/// The stages by layer, each layer in barycentre order. A stage's layer
/// is the longest path from a source; an input naming no stage of the
/// pipeline, or one a cycle kept from being placed first, adds nothing to
/// the path, so every stage lands in some layer even for a pipeline
/// `from_doc` would have refused.
pub fn layers(pipeline: &Pipeline) -> Vec<Vec<StageName>> {
    let stages = &pipeline.stages;
    let index = |name: &str| stages.iter().position(|s| s.name == name);

    let mut depth = vec![0usize; stages.len()];
    let mut placed = vec![false; stages.len()];
    for stage in pipeline.topological() {
        let i = index(&stage.name).expect("topological yields this pipeline's stages");
        depth[i] = stage
            .inputs
            .iter()
            .filter_map(|input| index(input))
            .filter(|&j| placed[j])
            .map(|j| depth[j] + 1)
            .max()
            .unwrap_or(0);
        placed[i] = true;
    }

    let count = depth.iter().max().map_or(0, |d| d + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (i, &d) in depth.iter().enumerate() {
        layers[d].push(i);
    }
    let mut row = vec![0usize; stages.len()];
    for layer in &layers {
        for (r, &i) in layer.iter().enumerate() {
            row[i] = r;
        }
    }

    // Down sweep: each layer under the mean row of its inputs.
    for layer in layers.iter_mut().skip(1) {
        sort_layer(layer, &mut row, |i, row| {
            mean(
                stages[i]
                    .inputs
                    .iter()
                    .filter_map(|input| index(input))
                    .map(|j| row[j]),
            )
        });
    }
    // Up sweep: each layer over the mean row of its consumers, from the
    // layer before the last down to the sources.
    for layer in layers.iter_mut().rev().skip(1) {
        sort_layer(layer, &mut row, |i, row| {
            let name = &stages[i].name;
            mean(
                stages
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.inputs.iter().any(|input| input == name))
                    .map(|(j, _)| row[j]),
            )
        });
    }

    layers
        .into_iter()
        .map(|layer| layer.into_iter().map(|i| stages[i].name.clone()).collect())
        .collect()
}

fn mean(rows: impl Iterator<Item = usize>) -> Option<f32> {
    let (sum, n) = rows.fold((0usize, 0usize), |(sum, n), r| (sum + r, n + 1));
    (n > 0).then(|| sum as f32 / n as f32)
}

/// Reorder one layer by `key` (given each stage and the current rows),
/// writing each stage's new row back. A stage with no key (nothing on the
/// swept side) keeps its current row as its key, and equal keys fall back
/// to document order.
fn sort_layer(
    layer: &mut [usize],
    row: &mut [usize],
    key: impl Fn(usize, &[usize]) -> Option<f32>,
) {
    let mut keyed: Vec<(f32, usize)> = layer
        .iter()
        .map(|&i| (key(i, row).unwrap_or(row[i] as f32), i))
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (r, (slot, (_, i))) in layer.iter_mut().zip(keyed).enumerate() {
        *slot = i;
        row[i] = r;
    }
}

/// Where `n` items of `size` go along a `span`: the first item's offset,
/// the step from one to the next, and the extent they cover. They spread
/// to fill the span while the gaps stay at or above `min_gap`; past that
/// the gaps hold `min_gap` and the extent exceeds the span. A single item
/// is centred.
fn spread(n: usize, size: f32, min_gap: f32, span: f32) -> (f32, f32, f32) {
    match n {
        0 => (0.0, 0.0, 0.0),
        1 => (((span - size) / 2.0).max(0.0), 0.0, size),
        _ => {
            let packed = n as f32 * size + (n - 1) as f32 * min_gap;
            if packed <= span {
                let gap = (span - n as f32 * size) / (n - 1) as f32;
                (0.0, size + gap, span)
            } else {
                (0.0, size + min_gap, packed)
            }
        }
    }
}

pub fn layout(input: LayoutInput<'_>) -> Layout {
    let LayoutInput {
        pipeline,
        width,
        height,
        rem,
    } = input;
    let card_w = CARD_W_REM * rem;
    let card_h = CARD_H_REM * rem;
    let min_gap = MIN_GAP_REM * rem;
    let caption_h = CAPTION_H_REM * rem;

    let layers = layers(pipeline);
    let (x0, step_x, extent_w) = spread(layers.len(), card_w, min_gap, width);
    let band = (height - caption_h).max(0.0);

    let mut cards = Vec::with_capacity(pipeline.stages.len());
    let mut captions = Vec::with_capacity(layers.len());
    let mut content_h = height;
    let mut fit_layers = 0;
    for (l, layer) in layers.iter().enumerate() {
        let (y0, step_y, extent_h) = spread(layer.len(), card_h, min_gap, band);
        content_h = content_h.max(caption_h + extent_h);
        let x = x0 + l as f32 * step_x;
        for (r, name) in layer.iter().enumerate() {
            cards.push(Card {
                stage: name.clone(),
                layer: l,
                row: r,
                x,
                y: caption_h + y0 + r as f32 * step_y,
            });
        }
        let kinds: Vec<&StageKind> = layer
            .iter()
            .filter_map(|name| pipeline.stage(name))
            .map(|s| &s.kind)
            .collect();
        captions.push((l, caption(&kinds, l, fit_layers + 1)));
        if l > 0 && kinds.iter().any(|k| is_fit(k)) {
            fit_layers += 1;
        }
    }

    Layout {
        cards,
        layers: layers.len(),
        card_w,
        card_h,
        content_w: width.max(extent_w),
        content_h,
        captions,
    }
}

fn is_fit(kind: &StageKind) -> bool {
    matches!(
        kind,
        StageKind::ParityFit { .. } | StageKind::KalmanFit { .. }
    )
}

fn is_derive(kind: &StageKind) -> bool {
    matches!(kind, StageKind::ImpliedRates | StageKind::Rasterize { .. })
}

/// The caption over a layer holding `kinds`. Layer 0 is `inputs` whatever
/// it holds. Elsewhere the distinct words of the kinds present join with
/// ` · ` in a fixed order: `stage {fit_ordinal} · fit` for a layer holding a
/// fit (`fit_ordinal` is the 1-based count of fit layers up to this one),
/// then `derive` (implied rates, rasterize), `check` and `publish`.
pub fn caption(kinds: &[&StageKind], layer: usize, fit_ordinal: usize) -> String {
    if layer == 0 {
        return "inputs".into();
    }
    let has = |pred: fn(&StageKind) -> bool| kinds.iter().any(|k| pred(k));
    let mut words: Vec<String> = Vec::with_capacity(4);
    if has(is_fit) {
        words.push(format!("stage {fit_ordinal} · fit"));
    }
    if has(is_derive) {
        words.push("derive".into());
    }
    if has(|k| matches!(k, StageKind::Check { .. })) {
        words.push("check".into());
    }
    if has(|k| matches!(k, StageKind::Publish { .. })) {
        words.push("publish".into());
    }
    words.join(" · ")
}

/// The stage a step from `from` lands on. `Left` is the first input in
/// port order, else the card of the previous layer nearest by y; `Right`
/// the first consumer in document order, else the nearest card of the
/// next layer; `Up` and `Down` the previous and next card of the same
/// layer, without wrapping. `None` when `from` has no card or the step
/// leaves the graph.
pub fn neighbour(
    layout: &Layout,
    pipeline: &Pipeline,
    from: &str,
    motion: Move,
) -> Option<StageName> {
    let here = layout.cards.iter().find(|c| c.stage == from)?;
    let has_card = |name: &str| layout.cards.iter().any(|c| c.stage == name);
    match motion {
        Move::Left => pipeline
            .stage(from)
            .and_then(|s| s.inputs.iter().find(|input| has_card(input)).cloned())
            .or_else(|| nearest(layout, here.layer.checked_sub(1)?, here.y)),
        Move::Right => pipeline
            .stages
            .iter()
            .find(|s| s.inputs.iter().any(|input| input == from) && has_card(&s.name))
            .map(|s| s.name.clone())
            .or_else(|| nearest(layout, here.layer + 1, here.y)),
        Move::Up => here
            .row
            .checked_sub(1)
            .and_then(|row| at(layout, here.layer, row)),
        Move::Down => at(layout, here.layer, here.row + 1),
    }
}

fn at(layout: &Layout, layer: usize, row: usize) -> Option<StageName> {
    layout
        .cards
        .iter()
        .find(|c| c.layer == layer && c.row == row)
        .map(|c| c.stage.clone())
}

/// The card of `layer` nearest to `y`; the earlier row on a tie.
fn nearest(layout: &Layout, layer: usize, y: f32) -> Option<StageName> {
    layout
        .cards
        .iter()
        .filter(|c| c.layer == layer)
        .min_by(|a, b| (a.y - y).abs().total_cmp(&(b.y - y).abs()))
        .map(|c| c.stage.clone())
}

/// The `n`th stage in layout order, 1-based.
pub fn nth(layout: &Layout, n: usize) -> Option<StageName> {
    layout.cards.get(n.checked_sub(1)?).map(|c| c.stage.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::builtin_pipeline;
    use geode_core::calibration::{Noise, RunMode, Stage};

    fn layer_of(layers: &[Vec<StageName>], name: &str) -> usize {
        layers
            .iter()
            .position(|layer| layer.iter().any(|s| s == name))
            .unwrap_or_else(|| panic!("{name} is in no layer"))
    }

    fn card<'a>(layout: &'a Layout, name: &str) -> &'a Card {
        layout
            .cards
            .iter()
            .find(|c| c.stage == name)
            .unwrap_or_else(|| panic!("{name} has no card"))
    }

    fn layout_of(pipeline: &Pipeline, width: f32, height: f32) -> Layout {
        layout(LayoutInput {
            pipeline,
            width,
            height,
            rem: 10.0,
        })
    }

    #[test]
    fn the_builtin_pipeline_lays_out_in_five_layers_with_sources_first() {
        let p = builtin_pipeline();
        let layers = layers(&p);
        assert_eq!(layers.len(), 6);
        for (name, layer) in [
            ("chain", 0),
            ("dividends", 0),
            ("rates", 0),
            ("parity", 1),
            ("implied_rates", 2),
            ("df_monotone", 2),
            ("repo_fit", 3),
            ("box_fit", 3),
            ("repo_curve", 4),
            ("box_curve", 4),
            ("publish", 5),
        ] {
            assert_eq!(layer_of(&layers, name), layer, "{name}");
        }
        let total: usize = layers.iter().map(Vec::len).sum();
        assert_eq!(total, p.stages.len());
    }

    #[test]
    fn barycentre_order_puts_a_stage_under_its_inputs() {
        let p = builtin_pipeline();
        let layers = layers(&p);
        // Sources keep document order: nothing above them orders them.
        assert_eq!(layers[0], ["chain", "dividends", "rates"]);
        // df_monotone reads parity alone (row 0); implied_rates reads
        // parity, dividends and rates (rows 0, 1, 2), so it sits lower
        // although the document names it first.
        assert_eq!(layers[2], ["df_monotone", "implied_rates"]);
        // Both fits read implied_rates: a tie, broken by document order.
        assert_eq!(layers[3], ["repo_fit", "box_fit"]);
        assert_eq!(layers[4], ["repo_curve", "box_curve"]);
    }

    #[test]
    fn columns_spread_to_the_width_and_overflow_past_the_minimum_gap() {
        let p = builtin_pipeline();
        // rem 10: cards 90 x 36, gap floor 20, captions 12.
        let wide = layout_of(&p, 1000.0, 500.0);
        assert_eq!(wide.layers, 6);
        assert_eq!(wide.card_w, 90.0);
        assert_eq!(wide.card_h, 36.0);
        assert_eq!(wide.content_w, 1000.0);
        assert_eq!(wide.content_h, 500.0);
        assert_eq!(card(&wide, "chain").x, 0.0);
        // Six columns of 90 leave 460 for five gaps of 92.
        assert_eq!(card(&wide, "parity").x, 182.0);
        assert_eq!(card(&wide, "publish").x, 910.0);
        // Three rows under a 12 px caption band: the first at the top of
        // the band, the last against the bottom, the middle halfway.
        assert_eq!(card(&wide, "chain").y, 12.0);
        assert_eq!(card(&wide, "dividends").y, 238.0);
        assert_eq!(card(&wide, "rates").y, 464.0);
        // A single row is centred in the band.
        assert_eq!(card(&wide, "parity").y, 238.0);

        // Six columns need 640; at 400 the gaps hold the floor and the
        // content runs past the viewport.
        let narrow = layout_of(&p, 400.0, 100.0);
        assert_eq!(narrow.content_w, 640.0);
        assert_eq!(card(&narrow, "parity").x - card(&narrow, "chain").x, 110.0);
        assert_eq!(card(&narrow, "publish").x, 550.0);
        // Three rows need 148 under the caption; 88 is left, so the rows
        // keep the floor and the content runs past the height.
        assert_eq!(card(&narrow, "chain").y, 12.0);
        assert_eq!(card(&narrow, "dividends").y, 68.0);
        assert_eq!(card(&narrow, "rates").y, 124.0);
        assert_eq!(narrow.content_h, 160.0);
        let layers = layers(&p);
        for c in &narrow.cards {
            assert_eq!(c.layer, layer_of(&layers, &c.stage), "{}", c.stage);
            assert_eq!(layers[c.layer][c.row], c.stage);
        }
    }

    #[test]
    fn captions_name_inputs_derive_and_numbered_fit_stages() {
        let p = builtin_pipeline();
        let l = layout_of(&p, 1000.0, 500.0);
        let captions: Vec<(usize, &str)> =
            l.captions.iter().map(|(i, c)| (*i, c.as_str())).collect();
        assert_eq!(
            captions,
            [
                (0, "inputs"),
                (1, "stage 1 · fit"),
                (2, "derive · check"),
                (3, "stage 2 · fit"),
                (4, "derive"),
                (5, "publish"),
            ]
        );
        let publish = StageKind::Publish {
            document: "rate_curve".into(),
            target: "sophis".into(),
        };
        let check = StageKind::Check {
            check: "monotone_df".into(),
        };
        let fit = StageKind::KalmanFit {
            series: "repo".into(),
            knots: vec![1.0],
            process_noise: 1.0,
            measurement_noise: Noise::Spread,
        };
        // Layer 0 is the inputs whatever it holds; elsewhere a fit takes
        // its ordinal and the other words follow in a fixed order.
        assert_eq!(caption(&[&publish], 0, 1), "inputs");
        assert_eq!(
            caption(&[&check, &publish, &StageKind::ImpliedRates], 3, 2),
            "derive · check · publish"
        );
        assert_eq!(caption(&[&check, &fit], 2, 3), "stage 3 · fit · check");
    }

    /// `a` feeds `c` and `d`; `c` feeds `e`; `d` feeds nothing.
    fn forked_pipeline() -> Pipeline {
        let stage = |name: &str, kind: StageKind, inputs: &[&str]| Stage {
            name: name.into(),
            kind,
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        };
        let fit = || StageKind::ParityFit {
            min_strikes: 6,
            strike_window: (0.8, 1.2),
        };
        Pipeline {
            name: "fork".into(),
            title: "fork".into(),
            run: RunMode::Manual,
            stages: vec![
                stage(
                    "a",
                    StageKind::Source {
                        dataset: "option_chain".into(),
                        series: None,
                    },
                    &[],
                ),
                stage("c", fit(), &["a"]),
                stage("d", fit(), &["a"]),
                stage(
                    "e",
                    StageKind::Check {
                        check: "monotone_df".into(),
                    },
                    &["c"],
                ),
            ],
        }
    }

    #[test]
    fn neighbour_follows_inputs_and_consumers_then_falls_back_to_nearest() {
        let p = builtin_pipeline();
        let l = layout_of(&p, 1000.0, 500.0);
        let go = |from: &str, m: Move| neighbour(&l, &p, from, m);
        // Left is the first input in port order; Right the first consumer
        // in document order.
        assert_eq!(go("publish", Move::Left).as_deref(), Some("repo_curve"));
        assert_eq!(go("implied_rates", Move::Left).as_deref(), Some("parity"));
        assert_eq!(go("parity", Move::Right).as_deref(), Some("implied_rates"));
        assert_eq!(go("df_monotone", Move::Right).as_deref(), Some("publish"));
        // Nothing before a source, nothing after the sink.
        assert_eq!(go("chain", Move::Left), None);
        assert_eq!(go("publish", Move::Right), None);
        // Up and Down walk the layer without wrapping.
        assert_eq!(go("chain", Move::Down).as_deref(), Some("dividends"));
        assert_eq!(go("dividends", Move::Up).as_deref(), Some("chain"));
        assert_eq!(go("chain", Move::Up), None);
        assert_eq!(go("rates", Move::Down), None);
        assert_eq!(
            go("df_monotone", Move::Down).as_deref(),
            Some("implied_rates")
        );
        assert_eq!(go("parity", Move::Down), None);
        assert_eq!(go("nowhere", Move::Right), None);

        // d has no consumer, so Right falls back to the nearest card of the
        // next layer by y: e, the only one.
        let f = forked_pipeline();
        let fl = layout_of(&f, 600.0, 300.0);
        assert_eq!(neighbour(&fl, &f, "d", Move::Right).as_deref(), Some("e"));
        assert_eq!(neighbour(&fl, &f, "c", Move::Right).as_deref(), Some("e"));
        assert_eq!(neighbour(&fl, &f, "e", Move::Right), None);
    }

    #[test]
    fn nth_walks_layout_order() {
        let p = builtin_pipeline();
        let l = layout_of(&p, 1000.0, 500.0);
        let names: Vec<StageName> = (1..=11).filter_map(|n| nth(&l, n)).collect();
        assert_eq!(
            names,
            [
                "chain",
                "dividends",
                "rates",
                "parity",
                "df_monotone",
                "implied_rates",
                "repo_fit",
                "box_fit",
                "repo_curve",
                "box_curve",
                "publish",
            ]
        );
        assert_eq!(nth(&l, 0), None);
        assert_eq!(nth(&l, 12), None);
    }
}
