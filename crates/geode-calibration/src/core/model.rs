//! The prepared render model: every string paint shows and every tone it
//! picks, computed once when the pipeline, its results, the layout or the
//! tile's cursor, filter or preview change. Paint reads it and formats
//! nothing. `build` allocates once per card and once per edge and never
//! otherwise, so it is cheap enough to rebuild on every delivery, and it
//! is never called per frame.

use std::collections::{BTreeMap, BTreeSet};

use geode_core::calibration::{Pipeline, StageKind, StageName, StageResult, StageStatus};
use geode_core::clock::Clock;
use geode_shell::palette::fuzzy_match;
use gpui::SharedString;

use super::layout::Layout;

/// The longest status line a card shows; a longer one is cut to this
/// many characters, ending in `…`, and kept whole in `status_full` for
/// the tooltip. A card is `CARD_W_REM` wide, which fits about this much
/// at the card's text size.
pub const STATUS_CHARS: usize = 20;

/// How a card paints: the stage's status, or `Waiting` for a source whose
/// document has not arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Failed,
    Held,
    Stale,
    Waiting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardModel {
    pub stage: StageName,
    /// The stage name.
    pub title: SharedString,
    /// `{kind} · {detail}`: the dataset of a source, the series of a fit,
    /// the check, the grid, the publish target; the kind alone when it
    /// has no detail.
    pub detail: SharedString,
    /// `{HH:MM:SS} · {summary}`, cut to `STATUS_CHARS`.
    pub status: SharedString,
    /// The status line whole.
    pub status_full: SharedString,
    pub tone: Tone,
    pub cursor: bool,
    /// An end of the preview edge.
    pub preview: bool,
    /// Fails the filter.
    pub dimmed: bool,
    pub x: f32,
    pub y: f32,
}

/// How an edge paints. `Preview` outranks every other tone, then `Gate`
/// (a check into a held publish, which explains the hold), then the
/// cursor's `Into` and `OutOf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTone {
    Quiet,
    Into,
    OutOf,
    Gate,
    Preview,
}

/// One edge, from the right middle of its input's card to the left middle
/// of its reader's.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeModel {
    pub from: StageName,
    pub to: StageName,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub tone: EdgeTone,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphModel {
    pub version: u64,
    pub layout: Layout,
    /// In layout order.
    pub cards: Vec<CardModel>,
    pub edges: Vec<EdgeModel>,
    /// Each layer's caption at its column's x.
    pub captions: Vec<(f32, String)>,
    /// `{cursor} · {status}`, else `waiting for {source}`, else empty.
    pub footer: SharedString,
}

pub struct ModelInput<'a> {
    pub pipeline: &'a Pipeline,
    pub results: &'a BTreeMap<StageName, StageResult>,
    /// Source stages whose document has not arrived.
    pub waiting: &'a BTreeSet<StageName>,
    pub cursor: Option<&'a str>,
    pub filter: Option<&'a str>,
    /// An edge the connect picker proposes: `(input, reader)`.
    pub preview_edge: Option<(&'a str, &'a str)>,
    pub layout: Layout,
    pub clock: Clock,
    pub version: u64,
}

pub fn build(input: ModelInput<'_>) -> GraphModel {
    let ModelInput {
        pipeline,
        results,
        waiting,
        cursor,
        filter,
        preview_edge,
        layout,
        clock,
        version,
    } = input;
    let half_h = layout.card_h / 2.0;
    let card_at = |name: &str| layout.cards.iter().find(|c| c.stage == name);
    let is_preview_end = |name: &str| preview_edge.is_some_and(|(a, b)| a == name || b == name);

    let mut cards = Vec::with_capacity(layout.cards.len());
    for card in &layout.cards {
        let Some(stage) = pipeline.stage(&card.stage) else {
            continue;
        };
        let name = card.stage.as_str();
        let result = results.get(name);
        let is_waiting = waiting.contains(name);
        let status_full = status_text(result, is_waiting, &clock);
        cards.push(CardModel {
            stage: card.stage.clone(),
            title: card.stage.clone().into(),
            detail: detail(&stage.kind).into(),
            status: shorten(&status_full).into(),
            status_full: status_full.into(),
            tone: tone(result, is_waiting),
            cursor: cursor == Some(name),
            preview: is_preview_end(name),
            dimmed: filter.is_some_and(|f| fuzzy_match(f, name).is_none()),
            x: card.x,
            y: card.y,
        });
    }

    let edge_count: usize = pipeline.stages.iter().map(|s| s.inputs.len()).sum();
    let mut edges = Vec::with_capacity(edge_count + 1);
    let is_held = |name: &str| {
        results
            .get(name)
            .is_some_and(|r| matches!(r.status, StageStatus::Held(_)))
    };
    for stage in &pipeline.stages {
        let Some(to) = card_at(&stage.name) else {
            continue;
        };
        for input in &stage.inputs {
            let Some(from) = card_at(input) else {
                continue;
            };
            let gates = matches!(stage.kind, StageKind::Publish { .. })
                && is_held(&stage.name)
                && pipeline
                    .stage(input)
                    .is_some_and(|s| matches!(s.kind, StageKind::Check { .. }));
            let tone = if preview_edge == Some((input.as_str(), stage.name.as_str())) {
                EdgeTone::Preview
            } else if gates {
                EdgeTone::Gate
            } else if cursor == Some(stage.name.as_str()) {
                EdgeTone::Into
            } else if cursor == Some(input.as_str()) {
                EdgeTone::OutOf
            } else {
                EdgeTone::Quiet
            };
            edges.push(EdgeModel {
                from: input.clone(),
                to: stage.name.clone(),
                x0: from.x + layout.card_w,
                y0: from.y + half_h,
                x1: to.x,
                y1: to.y + half_h,
                tone,
            });
        }
    }
    // A proposed edge the pipeline does not hold yet is drawn as well.
    if let Some((a, b)) = preview_edge
        && !edges.iter().any(|e| e.from == a && e.to == b)
        && let (Some(from), Some(to)) = (card_at(a), card_at(b))
    {
        edges.push(EdgeModel {
            from: a.into(),
            to: b.into(),
            x0: from.x + layout.card_w,
            y0: from.y + half_h,
            x1: to.x,
            y1: to.y + half_h,
            tone: EdgeTone::Preview,
        });
    }

    let captions = layout
        .captions
        .iter()
        .map(|(l, text)| {
            let x = layout
                .cards
                .iter()
                .find(|c| c.layer == *l)
                .map_or(0.0, |c| c.x);
            (x, text.clone())
        })
        .collect();

    let footer: SharedString =
        if let Some(at) = cursor.and_then(|c| cards.iter().find(|m| m.stage == c)) {
            format!("{} · {}", at.stage, at.status_full).into()
        } else if let Some(source) = layout
            .cards
            .iter()
            .find(|c| waiting.contains(c.stage.as_str()))
        {
            format!("waiting for {}", source.stage).into()
        } else {
            SharedString::default()
        };

    GraphModel {
        version,
        layout,
        cards,
        edges,
        captions,
        footer,
    }
}

fn detail(kind: &StageKind) -> String {
    let specific = match kind {
        StageKind::Source { dataset, .. } => Some(dataset.as_str()),
        StageKind::KalmanFit { series, .. } => Some(series.as_str()),
        StageKind::Check { check } => Some(check.as_str()),
        StageKind::Rasterize { grid } => Some(grid.as_str()),
        StageKind::Publish { target, .. } => Some(target.as_str()),
        StageKind::ParityFit { .. } | StageKind::ImpliedRates => None,
    };
    match specific {
        Some(d) => format!("{} · {d}", kind.kind_name()),
        None => kind.kind_name().to_string(),
    }
}

/// `{HH:MM:SS} · {summary}` when the stage ran, the summary alone when it
/// has a result but no time (skipped, stale); `waiting` for a source not
/// yet arrived and `not run` for a stage with no result.
fn status_text(result: Option<&StageResult>, waiting: bool, clock: &Clock) -> String {
    match result {
        Some(r) => {
            let summary = if r.status == StageStatus::Skipped {
                "skipped"
            } else {
                r.summary.as_str()
            };
            match r.ran_at {
                Some(t) => format!("{} · {summary}", clock.hms(t)),
                None => summary.to_string(),
            }
        }
        None if waiting => "waiting".into(),
        None => "not run".into(),
    }
}

fn tone(result: Option<&StageResult>, waiting: bool) -> Tone {
    if waiting {
        return Tone::Waiting;
    }
    match result.map(|r| &r.status) {
        Some(StageStatus::Failed(_)) => Tone::Failed,
        Some(StageStatus::Held(_)) => Tone::Held,
        Some(StageStatus::Stale) => Tone::Stale,
        Some(StageStatus::Ok | StageStatus::Skipped) | None => Tone::Plain,
    }
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= STATUS_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(STATUS_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layout::{LayoutInput, layout};
    use crate::core::testing::builtin_pipeline;
    use chrono::{TimeZone, Utc};
    use geode_core::calibration::StagePayload;
    use std::time::Duration;

    fn result(name: &str, status: StageStatus, ran: bool, summary: &str) -> StageResult {
        StageResult {
            name: name.into(),
            status,
            ran_at: ran.then(|| Utc.with_ymd_and_hms(2026, 11, 9, 9, 30, 15).unwrap()),
            elapsed: Duration::from_millis(3),
            summary: summary.into(),
            payload: StagePayload::None,
        }
    }

    fn results(list: Vec<StageResult>) -> BTreeMap<StageName, StageResult> {
        list.into_iter().map(|r| (r.name.clone(), r)).collect()
    }

    fn layout_of(pipeline: &Pipeline) -> Layout {
        layout(LayoutInput {
            pipeline,
            width: 1000.0,
            height: 500.0,
            rem: 10.0,
        })
    }

    struct Scene<'a> {
        pipeline: &'a Pipeline,
        results: BTreeMap<StageName, StageResult>,
        waiting: BTreeSet<StageName>,
        cursor: Option<&'a str>,
        filter: Option<&'a str>,
        preview_edge: Option<(&'a str, &'a str)>,
    }

    impl<'a> Scene<'a> {
        fn new(pipeline: &'a Pipeline) -> Self {
            Scene {
                pipeline,
                results: BTreeMap::new(),
                waiting: BTreeSet::new(),
                cursor: None,
                filter: None,
                preview_edge: None,
            }
        }

        fn build(&self) -> GraphModel {
            build(ModelInput {
                pipeline: self.pipeline,
                results: &self.results,
                waiting: &self.waiting,
                cursor: self.cursor,
                filter: self.filter,
                preview_edge: self.preview_edge,
                layout: layout_of(self.pipeline),
                clock: Clock::utc(),
                version: 7,
            })
        }
    }

    fn card<'a>(model: &'a GraphModel, name: &str) -> &'a CardModel {
        model
            .cards
            .iter()
            .find(|c| c.stage == name)
            .unwrap_or_else(|| panic!("{name} has no card"))
    }

    fn edge<'a>(model: &'a GraphModel, from: &str, to: &str) -> &'a EdgeModel {
        model
            .edges
            .iter()
            .find(|e| e.from == from && e.to == to)
            .unwrap_or_else(|| panic!("no edge {from} -> {to}"))
    }

    #[test]
    fn cards_carry_tone_time_and_summary_from_results() {
        let p = builtin_pipeline();
        let mut scene = Scene::new(&p);
        scene.results = results(vec![
            result("parity", StageStatus::Ok, true, "6 strikes"),
            result(
                "publish",
                StageStatus::Held("df_monotone: 1 violation".into()),
                true,
                "held by df_monotone",
            ),
            result("repo_fit", StageStatus::Skipped, false, ""),
            result(
                "box_fit",
                StageStatus::Failed("no points".into()),
                true,
                "no points",
            ),
            result("repo_curve", StageStatus::Stale, false, "stale"),
            result(
                "df_monotone",
                StageStatus::Ok,
                true,
                "a summary long enough to run past the card",
            ),
        ]);
        scene.waiting.insert("rates".into());
        let m = scene.build();
        assert_eq!(m.version, 7);
        assert_eq!(m.cards.len(), 11);

        let parity = card(&m, "parity");
        assert_eq!(parity.title.as_ref(), "parity");
        assert_eq!(parity.detail.as_ref(), "parity_fit");
        assert_eq!(parity.status.as_ref(), "09:30:15 · 6 strikes");
        assert_eq!(parity.status_full, parity.status);
        assert_eq!(parity.tone, Tone::Plain);
        assert_eq!((parity.x, parity.y), (182.0, 238.0));

        let publish = card(&m, "publish");
        assert_eq!(publish.detail.as_ref(), "publish · sophis");
        assert_eq!(publish.tone, Tone::Held);
        assert_eq!(
            publish.status_full.as_ref(),
            "09:30:15 · held by df_monotone"
        );
        // The check into the held publish is the gate; the rasters are not.
        assert_eq!(edge(&m, "df_monotone", "publish").tone, EdgeTone::Gate);
        assert_eq!(edge(&m, "repo_curve", "publish").tone, EdgeTone::Quiet);
        assert_eq!(edge(&m, "box_curve", "publish").tone, EdgeTone::Quiet);

        let repo_fit = card(&m, "repo_fit");
        assert_eq!(repo_fit.detail.as_ref(), "kalman_fit · repo");
        assert_eq!(repo_fit.tone, Tone::Plain);
        assert_eq!(repo_fit.status.as_ref(), "skipped");
        assert_eq!(card(&m, "box_fit").tone, Tone::Failed);
        assert_eq!(card(&m, "box_fit").status.as_ref(), "09:30:15 · no points");
        let repo_curve = card(&m, "repo_curve");
        assert_eq!(repo_curve.detail.as_ref(), "rasterize · monthly_to_5y");
        assert_eq!(repo_curve.tone, Tone::Stale);
        assert_eq!(repo_curve.status.as_ref(), "stale");

        // A source that has not arrived waits; one with no result and no
        // wait has not run.
        let rates = card(&m, "rates");
        assert_eq!(rates.detail.as_ref(), "source · rate_curve");
        assert_eq!(rates.tone, Tone::Waiting);
        assert_eq!(rates.status.as_ref(), "waiting");
        let chain = card(&m, "chain");
        assert_eq!(chain.detail.as_ref(), "source · option_chain");
        assert_eq!(chain.tone, Tone::Plain);
        assert_eq!(chain.status.as_ref(), "not run");
        assert_eq!(card(&m, "implied_rates").detail.as_ref(), "implied_rates");

        // A long status is cut for the card and kept whole for the tooltip.
        let check = card(&m, "df_monotone");
        assert_eq!(check.detail.as_ref(), "check · monotone_df");
        assert_eq!(
            check.status_full.as_ref(),
            "09:30:15 · a summary long enough to run past the card"
        );
        assert_eq!(check.status.chars().count(), STATUS_CHARS);
        assert!(check.status.ends_with('…'), "{}", check.status);
        assert!(
            check
                .status_full
                .starts_with(check.status.trim_end_matches('…'))
        );

        // Edges run from the right middle of the input to the left middle
        // of the reader.
        let e = edge(&m, "parity", "implied_rates");
        let parity_card = card(&m, "parity");
        let implied = card(&m, "implied_rates");
        assert_eq!((e.x0, e.y0), (parity_card.x + 90.0, parity_card.y + 18.0));
        assert_eq!((e.x1, e.y1), (implied.x, implied.y + 18.0));
        assert_eq!(m.edges.len(), 12);

        // Captions sit at their column's x.
        let captions: Vec<(f32, &str)> = m.captions.iter().map(|(x, c)| (*x, c.as_str())).collect();
        assert_eq!(captions[0], (0.0, "inputs"));
        assert_eq!(captions[1], (182.0, "stage 1 · fit"));
        assert_eq!(captions[5], (910.0, "publish"));
        assert!(!m.cards.iter().any(|c| c.cursor || c.preview || c.dimmed));
    }

    #[test]
    fn edges_into_and_out_of_the_cursor_take_their_tones() {
        let p = builtin_pipeline();
        let mut scene = Scene::new(&p);
        scene.cursor = Some("implied_rates");
        let m = scene.build();
        assert!(card(&m, "implied_rates").cursor);
        assert_eq!(m.cards.iter().filter(|c| c.cursor).count(), 1);
        assert_eq!(edge(&m, "parity", "implied_rates").tone, EdgeTone::Into);
        assert_eq!(edge(&m, "dividends", "implied_rates").tone, EdgeTone::Into);
        assert_eq!(edge(&m, "rates", "implied_rates").tone, EdgeTone::Into);
        assert_eq!(edge(&m, "implied_rates", "repo_fit").tone, EdgeTone::OutOf);
        assert_eq!(edge(&m, "implied_rates", "box_fit").tone, EdgeTone::OutOf);
        assert_eq!(edge(&m, "chain", "parity").tone, EdgeTone::Quiet);
        assert_eq!(edge(&m, "parity", "df_monotone").tone, EdgeTone::Quiet);
    }

    #[test]
    fn a_filter_dims_non_matching_cards_and_a_preview_edge_is_added() {
        let p = builtin_pipeline();
        let mut scene = Scene::new(&p);
        scene.filter = Some("fit");
        scene.preview_edge = Some(("implied_rates", "df_monotone"));
        let m = scene.build();
        let lit: Vec<&str> = m
            .cards
            .iter()
            .filter(|c| !c.dimmed)
            .map(|c| c.stage.as_str())
            .collect();
        assert_eq!(lit, ["repo_fit", "box_fit"]);
        // The preview is a thirteenth edge, drawn like the others.
        assert_eq!(m.edges.len(), 13);
        let preview = edge(&m, "implied_rates", "df_monotone");
        assert_eq!(preview.tone, EdgeTone::Preview);
        let from = card(&m, "implied_rates");
        let to = card(&m, "df_monotone");
        assert_eq!((preview.x0, preview.y0), (from.x + 90.0, from.y + 18.0));
        assert_eq!((preview.x1, preview.y1), (to.x, to.y + 18.0));
        assert!(from.preview && to.preview);
        assert_eq!(m.cards.iter().filter(|c| c.preview).count(), 2);

        // A preview over an edge that already exists recolours it.
        scene.filter = None;
        scene.preview_edge = Some(("parity", "df_monotone"));
        let m = scene.build();
        assert_eq!(m.edges.len(), 12);
        assert_eq!(edge(&m, "parity", "df_monotone").tone, EdgeTone::Preview);
        assert!(m.cards.iter().all(|c| !c.dimmed));
    }

    #[test]
    fn the_footer_names_the_cursor_or_the_missing_source() {
        let p = builtin_pipeline();
        let mut scene = Scene::new(&p);
        scene.results = results(vec![result("parity", StageStatus::Ok, true, "6 strikes")]);
        scene.cursor = Some("parity");
        assert_eq!(
            scene.build().footer.as_ref(),
            "parity · 09:30:15 · 6 strikes"
        );

        scene.cursor = None;
        scene.waiting.insert("rates".into());
        scene.waiting.insert("dividends".into());
        // The first waiting source in layout order.
        assert_eq!(scene.build().footer.as_ref(), "waiting for dividends");

        // The cursor's line comes first even while a source waits.
        scene.cursor = Some("rates");
        assert_eq!(scene.build().footer.as_ref(), "rates · waiting");

        scene.cursor = None;
        scene.waiting.clear();
        assert_eq!(scene.build().footer.as_ref(), "");
    }
}
