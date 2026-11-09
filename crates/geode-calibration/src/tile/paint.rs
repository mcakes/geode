//! Paint: the graph body and the footer. Everything painted is read off
//! the prepared model (`core::model`) and the strings prepared beside it
//! ([`Chrome`], rebuilt with the model; [`Hints`], rebuilt on a keymap
//! change); render formats nothing and allocates only what GPUI's element
//! tree needs.
//!
//! The body is a clipped surface the tile scrolls by its own offset. A
//! `canvas` under the cards strokes every edge as a cubic Bézier whose
//! control points sit at the horizontal midpoint ([`edge_curve`]), ending
//! in a small disc at the reader; the cards are absolutely placed `div`s
//! over it, so an edge runs under a card rather than across it. The
//! canvas's prepaint is also where the body's size is measured: a changed
//! size is handed to the tile after the draw (`cx.defer`), which rebuilds
//! the model at the real viewport once rather than per frame.

use gpui::prelude::*;
use gpui::{
    App, Div, ElementId, Entity, Hsla, PathBuilder, Pixels, SharedString, Stateful, WeakEntity,
    Window, canvas, div, point, px,
};
use gpui_component::input::Input;
use gpui_component::theme::Theme;
use gpui_component::{h_flex, v_flex};

use geode_shell::keymap::{Keystroke, Modifiers, parse_binding};
use geode_shell::shell::{kbd, scale};
use geode_shell::tips::{self, Chords, chord_for_preferring};

use super::CalibrationTile;
use super::keys::Viewport;
use crate::core::model::{EdgeModel, EdgeTone, GraphModel, Tone};

/// The opacity of a card that fails the filter.
const DIMMED: f32 = 0.35;
/// The stroke of an edge and the radius of its end disc, in pixels at the
/// design rem; both scale with the window's rem.
const EDGE_STROKE: f32 = 1.0;
const DISC_RADIUS: f32 = 2.5;
/// The dash of a gate or preview edge, in pixels at the design rem.
const DASH: f32 = 4.0;
/// The end disc as a polygon: enough sides to read as round at its size.
const DISC_SIDES: usize = 12;

/// The footer hints, each verb's chords then its word, in the fixed order
/// the row reads: what the graph does first, the verbs after. A verb the
/// keymap does not bind (one not yet carried by a binding) paints no hint,
/// so the row fills in as bindings register.
const NORMAL_HINTS: &[(&[(&str, &str)], &str)] = &[
    (
        &[
            ("calibration::cursor_left", "h"),
            ("calibration::cursor_down", "j"),
            ("calibration::cursor_up", "k"),
            ("calibration::cursor_right", "l"),
        ],
        "move",
    ),
    (&[("calibration::filter", "/")], "filter"),
    (&[("calibration::run_from", "r")], "run"),
    (&[("calibration::run_all", "shift+r")], "run all"),
    (&[("calibration::publish", "p")], "publish"),
    (&[("calibration::connect", "c")], "connect"),
    (&[("calibration::disconnect", "x")], "disconnect"),
    (&[("calibration::add", "a")], "add"),
    (&[("calibration::delete", "d")], "delete"),
    (&[("calibration::menu", ".")], "menu"),
];

/// The hints while the filter field is up.
const INSERT_HINTS: &[(&[(&str, &str)], &str)] = &[
    (&[("calibration::commit", "enter")], "keep"),
    (&[("calibration::cancel", "escape")], "revert"),
];

/// The hints while a picker list is up: the shared steps are the shell's
/// (`tilelist`), named by their motion ids.
const LIST_HINTS: &[(&[(&str, &str)], &str)] = &[
    (
        &[
            (geode_tile::motion::MENU_DOWN, "j"),
            (geode_tile::motion::MENU_UP, "k"),
        ],
        "move",
    ),
    (&[("calibration::commit", "enter")], "pick"),
    (&[("calibration::cancel", "escape")], "close"),
];

/// The hints while the add picker's name field is up.
const NAME_HINTS: &[(&[(&str, &str)], &str)] = &[
    (&[("calibration::commit", "enter")], "add"),
    (&[("calibration::cancel", "escape")], "cancel"),
];

/// One footer hint: its chords, then its word.
pub(super) type FooterHint = (Vec<Vec<Keystroke>>, SharedString);

/// The footer's hint rows, resolved against the live keymap on a `Chords`
/// change, never per frame.
#[derive(Default)]
pub(super) struct Hints {
    pub(super) normal: Vec<FooterHint>,
    pub(super) insert: Vec<FooterHint>,
    pub(super) list: Vec<FooterHint>,
    pub(super) name: Vec<FooterHint>,
}

impl Hints {
    pub(super) fn resolve(cx: &App) -> Hints {
        let empty = Vec::new();
        let bindings = cx
            .try_global::<Chords>()
            .map(|c| c.0.as_slice())
            .unwrap_or(&empty);
        let row = |table: &'static [(&'static [(&'static str, &'static str)], &'static str)]| -> Vec<FooterHint> {
            table
                .iter()
                .filter_map(|(actions, word)| {
                    let keys: Vec<Vec<Keystroke>> = actions
                        .iter()
                        .filter_map(|(action, shipped)| {
                            let shipped = parse_binding(shipped, Modifiers::NONE)
                                .expect("shipped footer keys are valid");
                            chord_for_preferring(bindings, action, &shipped)
                        })
                        .collect();
                    (!keys.is_empty()).then(|| (keys, SharedString::new_static(word)))
                })
                .collect()
        };
        Hints {
            normal: row(NORMAL_HINTS),
            insert: row(INSERT_HINTS),
            list: row(LIST_HINTS),
            name: row(NAME_HINTS),
        }
    }
}

/// The element names of one card, prepared with the model so paint
/// formats nothing per frame.
pub(super) struct CardIds {
    pub(super) id: ElementId,
    pub(super) selector: SharedString,
    pub(super) tip_selector: SharedString,
}

/// The per-card names and the caption texts, rebuilt with the model.
#[derive(Default)]
pub(super) struct Chrome {
    pub(super) cards: Vec<CardIds>,
    pub(super) captions: Vec<SharedString>,
}

impl Chrome {
    pub(super) fn prepare(tile: u64, model: &GraphModel) -> Chrome {
        Chrome {
            cards: model
                .cards
                .iter()
                .map(|c| CardIds {
                    id: ElementId::Name(format!("calibration-card-{tile}-{}", c.stage).into()),
                    selector: format!("calibration-card-{tile}-{}", c.stage).into(),
                    tip_selector: format!("tip-calibration-card-{tile}-{}", c.stage).into(),
                })
                .collect(),
            captions: model
                .captions
                .iter()
                .map(|(_, text)| SharedString::from(text.clone()))
                .collect(),
        }
    }
}

/// A cubic Bézier from an edge's input end to its reader end, both
/// control points at the horizontal midpoint: the curve leaves and
/// arrives level, so a fan of edges into one card reads as one bundle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Curve {
    pub p0: (f32, f32),
    pub c1: (f32, f32),
    pub c2: (f32, f32),
    pub p1: (f32, f32),
}

pub(crate) fn edge_curve(edge: &EdgeModel) -> Curve {
    let mid = (edge.x0 + edge.x1) / 2.0;
    Curve {
        p0: (edge.x0, edge.y0),
        c1: (mid, edge.y0),
        c2: (mid, edge.y1),
        p1: (edge.x1, edge.y1),
    }
}

/// The edge tones' colors, read from the theme once per paint.
#[derive(Clone, Copy)]
struct EdgeInk {
    into: Hsla,
    out_of: Hsla,
    gate: Hsla,
    preview: Hsla,
    quiet: Hsla,
}

impl EdgeInk {
    fn of(theme: &Theme) -> EdgeInk {
        EdgeInk {
            into: theme.foreground,
            out_of: theme.ring,
            gate: theme.warning,
            preview: theme.ring,
            quiet: theme.border,
        }
    }

    fn color(&self, tone: EdgeTone) -> Hsla {
        match tone {
            EdgeTone::Into => self.into,
            EdgeTone::OutOf => self.out_of,
            EdgeTone::Gate => self.gate,
            EdgeTone::Preview => self.preview,
            EdgeTone::Quiet => self.quiet,
        }
    }
}

fn dashed(tone: EdgeTone) -> bool {
    matches!(tone, EdgeTone::Gate | EdgeTone::Preview)
}

/// Paint every edge of `model` at `origin` less the scroll.
fn paint_edges(
    model: &GraphModel,
    origin: gpui::Point<Pixels>,
    scroll: (f32, f32),
    ink: EdgeInk,
    rem: Pixels,
    window: &mut Window,
) {
    let stroke = px(scale::design_px(EDGE_STROKE, rem));
    let dash = px(scale::design_px(DASH, rem));
    let radius = scale::design_px(DISC_RADIUS, rem);
    let at = |x: f32, y: f32| point(origin.x + px(x - scroll.0), origin.y + px(y - scroll.1));
    for edge in &model.edges {
        let curve = edge_curve(edge);
        let mut path = PathBuilder::stroke(stroke);
        if dashed(edge.tone) {
            path = path.dash_array(&[dash, dash]);
        }
        path.move_to(at(curve.p0.0, curve.p0.1));
        path.cubic_bezier_to(
            at(curve.p1.0, curve.p1.1),
            at(curve.c1.0, curve.c1.1),
            at(curve.c2.0, curve.c2.1),
        );
        let color = ink.color(edge.tone);
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        // The disc marks the reader's end.
        let centre = at(curve.p1.0, curve.p1.1);
        let mut points = [centre; DISC_SIDES];
        for (i, p) in points.iter_mut().enumerate() {
            let a = i as f32 / DISC_SIDES as f32 * std::f32::consts::TAU;
            *p = point(
                centre.x + px(radius * a.cos()),
                centre.y + px(radius * a.sin()),
            );
        }
        let mut disc = PathBuilder::fill();
        disc.add_polygon(&points, true);
        if let Ok(disc) = disc.build() {
            window.paint_path(disc, color);
        }
    }
}

/// A card's border and status colors by tone.
fn card_ink(tone: Tone, theme: &Theme) -> (Hsla, Hsla) {
    match tone {
        Tone::Plain => (theme.border, theme.muted_foreground),
        Tone::Failed => (theme.danger, theme.danger),
        Tone::Held => (theme.warning, theme.warning),
        Tone::Stale => (theme.muted_foreground, theme.muted_foreground),
        Tone::Waiting => (theme.info, theme.info),
    }
}

impl CalibrationTile {
    /// The graph: the clipped, scrolled surface with the edge canvas, the
    /// captions and the cards, and the edit picker hung under the cursor's
    /// card while one is up.
    pub(super) fn render_graph(
        &self,
        tile: &Entity<Self>,
        model: &std::rc::Rc<GraphModel>,
        theme: &Theme,
        window: &Window,
        cx: &App,
    ) -> Div {
        let id = self.id.0;
        let (sx, sy) = self.scroll;
        let layout = &model.layout;
        let rem = window.rem_size();
        let caption_h = crate::core::layout::CAPTION_H_REM * rem.as_f32();
        let known = self.viewport;
        let weak: WeakEntity<Self> = tile.downgrade();
        let ink = EdgeInk::of(theme);
        let edges = std::rc::Rc::clone(model);
        let canvas = canvas(
            move |bounds, window, cx| {
                let measured = Viewport {
                    width: bounds.size.width.as_f32(),
                    height: bounds.size.height.as_f32(),
                    rem: window.rem_size().as_f32(),
                };
                // A change is applied after this draw: a notify made
                // during one is dropped.
                if measured != known {
                    let weak = weak.clone();
                    cx.defer(move |cx| {
                        let _ = weak.update(cx, |t, cx| t.viewport_measured(measured, cx));
                    });
                }
                bounds.origin
            },
            move |_, origin, window, _| paint_edges(&edges, origin, (sx, sy), ink, rem, window),
        )
        .absolute()
        .inset_0();
        let mut body = div()
            .relative()
            .overflow_hidden()
            .flex_1()
            .min_h_0()
            .w_full()
            .debug_selector(move || format!("calibration-graph-{id}"))
            .child(canvas);
        for ((x, _), text) in model.captions.iter().zip(&self.chrome.captions) {
            body = body.child(
                div()
                    .absolute()
                    .left(px(x - sx))
                    .top(px(-sy))
                    .h(px(caption_h))
                    .w(px(layout.card_w))
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(text.clone()),
            );
        }
        for (card, ids) in model.cards.iter().zip(&self.chrome.cards) {
            body = body.child(render_card(card, ids, layout, (sx, sy), theme));
        }
        // The picker hangs by its top-left corner from the cursor card's
        // bottom-left (the deferred anchor lifts it over the body's clip);
        // with no cursor's card (an add on an empty cursor) from the first
        // row's top-left.
        if let Some(picker) = self.render_picker(tile, cx) {
            let at = self
                .cursor
                .as_deref()
                .and_then(|c| layout.cards.iter().find(|k| k.stage == c))
                .map(|k| (k.x, k.y + layout.card_h))
                .unwrap_or((0.0, caption_h));
            body = body.child(
                div()
                    .absolute()
                    .left(px(at.0 - sx))
                    .top(px(at.1 - sy))
                    .child(picker),
            );
        }
        body
    }

    /// The footer: the filter field while it is up, else the model's
    /// line; the hint row at the right either way, for whatever holds the
    /// keys (the filter, a picker list, the name field, else the graph).
    pub(super) fn render_footer(&self, model: Option<&GraphModel>, theme: &Theme) -> Div {
        let id = self.id.0;
        let muted = theme.muted_foreground;
        let hints = if self.filter_field.is_some() {
            &self.hints.insert
        } else if self.picker_field_up() {
            &self.hints.name
        } else if self.picker_list_up() {
            &self.hints.list
        } else {
            &self.hints.normal
        };
        let left: gpui::AnyElement = match &self.filter_field {
            Some(field) => h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_center()
                .text_color(theme.foreground)
                .debug_selector(move || format!("calibration-filter-{id}"))
                .child(div().flex_none().text_color(muted).child("/"))
                .child(Input::new(&field.input).appearance(false).w_full())
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .debug_selector(move || format!("calibration-footer-text-{id}"))
                .child(model.map(|m| m.footer.clone()).unwrap_or_default())
                .into_any_element(),
        };
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .text_xs()
            .text_color(muted)
            .border_t_1()
            .border_color(theme.border)
            .debug_selector(move || format!("calibration-footer-{id}"))
            .child(left)
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .items_center()
                    .overflow_hidden()
                    .children(hints.iter().map(|(bindings, word)| {
                        h_flex()
                            .gap_1()
                            .items_center()
                            .flex_shrink_0()
                            .children(bindings.iter().map(|keys| kbd::binding(keys)))
                            .child(word.clone())
                    })),
            )
    }
}

/// One card at its place less the scroll: the stage name, its kind and
/// detail, its status line (the whole line in a tooltip when it was
/// cut). The tone colors the border and the status; the cursor's card
/// takes the ring and the list's active fill; a preview end a dashed
/// ring; a card failing the filter paints dimmed.
fn render_card(
    card: &crate::core::model::CardModel,
    ids: &CardIds,
    layout: &crate::core::layout::Layout,
    scroll: (f32, f32),
    theme: &Theme,
) -> Stateful<Div> {
    let (border, status) = card_ink(card.tone, theme);
    let selector = ids.selector.clone();
    let cut = card.status != card.status_full;
    v_flex()
        .id(ids.id.clone())
        .absolute()
        .left(px(card.x - scroll.0))
        .top(px(card.y - scroll.1))
        .w(px(layout.card_w))
        .h(px(layout.card_h))
        .overflow_hidden()
        .justify_center()
        .px_1p5()
        .py_0p5()
        .rounded(theme.radius)
        .bg(theme.background)
        .border_1()
        .border_color(border)
        .when(card.cursor, |el| {
            el.border_2().border_color(theme.ring).bg(theme.list_active)
        })
        .when(card.preview && !card.cursor, |el| {
            el.border_dashed().border_color(theme.ring)
        })
        .when(card.dimmed, |el| el.opacity(DIMMED))
        .debug_selector(move || selector.to_string())
        .when(cut, |el| {
            el.tooltip(tips::tip_with(
                ids.tip_selector.clone(),
                card.status_full.clone(),
                None,
                None,
            ))
        })
        .child(
            div()
                .text_sm()
                .text_color(theme.foreground)
                .truncate()
                .child(card.title.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .truncate()
                .child(card.detail.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(status)
                .when(card.tone == Tone::Stale, |el| el.italic())
                .truncate()
                .child(card.status.clone()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper the canvas strokes from: both control points at the
    /// horizontal midpoint, each level with its own end.
    #[test]
    fn edge_paths_are_cubic_with_midpoint_controls() {
        let edge = EdgeModel {
            from: "a".into(),
            to: "b".into(),
            x0: 100.0,
            y0: 50.0,
            x1: 300.0,
            y1: 150.0,
            tone: EdgeTone::Quiet,
        };
        assert_eq!(
            edge_curve(&edge),
            Curve {
                p0: (100.0, 50.0),
                c1: (200.0, 50.0),
                c2: (200.0, 150.0),
                p1: (300.0, 150.0),
            }
        );
        // A level edge is a straight line through its controls.
        let level = EdgeModel { y1: 50.0, ..edge };
        let c = edge_curve(&level);
        assert_eq!((c.c1.1, c.c2.1), (50.0, 50.0));
        assert_eq!(c.c1.0, c.c2.0);
        assert!(dashed(EdgeTone::Gate) && dashed(EdgeTone::Preview));
        assert!(!dashed(EdgeTone::Into) && !dashed(EdgeTone::OutOf) && !dashed(EdgeTone::Quiet));
    }
}
