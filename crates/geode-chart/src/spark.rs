//! A sparkline or term glyph for a grid cell: a decimated polyline with an
//! optional end dot and marked point, on its own or a shared y-range. No
//! axes, grid, labels or tooltip; the host paints the number beside it and
//! supplies every color. Fills its parent, like the kit's other elements.

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{App, Bounds, ContentMask, Hsla, Pixels, Window, fill, point, px, size};
use gpui_component::plot::{IntoPlot, Plot};

use crate::core::spark::{SparkScratch, layout};
use crate::core::{Point, Rect, design_px};
use crate::paint::{LINE_WIDTH, stroke_points};

/// End-dot radius, mark radius and ring width at the design rem (the
/// mockup's 2.2 px dot with a 1 px ring at a 13 px base).
const END_RADIUS: f32 = 2.0;
const MARK_RADIUS: f32 = 1.75;
const RING: f32 = 1.0;

/// Cell sizes a host gives a `Spark`, in design px at the 12 px design rem:
/// the element fills its parent, so the host sizes the wrapper from these.
pub mod sizes {
    pub const HEIGHT: f32 = 16.0;
    /// A 30-day daily series.
    pub const WIDTH_30D: f32 = 72.0;
    /// A term glyph.
    pub const WIDTH_TERM: f32 = 44.0;
    /// A one-year daily series.
    pub const WIDTH_1Y: f32 = 140.0;
}

/// Colors the host resolves once per theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SparkPaint {
    pub line: Hsla,
    pub end: Hsla,
    pub mark: Hsla,
    /// Painted under the end dot so it reads on any row tint.
    pub ring: Hsla,
}

thread_local! {
    /// One layout scratch per thread: elements are rebuilt every render, so
    /// a field could not keep its capacity; a grid of sparklines reuses this.
    static SCRATCH: RefCell<SparkScratch> = RefCell::new(SparkScratch::default());
}

#[derive(IntoPlot)]
pub struct Spark {
    values: Arc<[f64]>,
    range: Option<(f64, f64)>,
    mark: Option<usize>,
    end_dot: bool,
    paint: SparkPaint,
}

impl Spark {
    /// A sparkline over `values` on their own range, with an end dot.
    pub fn new(values: Arc<[f64]>, paint: SparkPaint) -> Self {
        Self {
            values,
            range: None,
            mark: None,
            end_dot: true,
            paint,
        }
    }

    /// A y-range shared across a column (`core::spark::shared_range`);
    /// `None` uses the series' own.
    pub fn range(mut self, range: Option<(f64, f64)>) -> Self {
        self.range = range;
        self
    }

    /// Mark the point at `index` (the term glyph's 3M).
    pub fn mark(mut self, index: Option<usize>) -> Self {
        self.mark = index;
        self
    }

    pub fn end_dot(mut self, on: bool) -> Self {
        self.end_dot = on;
        self
    }
}

fn dot(window: &mut Window, at: Point, radius: f32, color: Hsla) {
    let d = px(radius * 2.0);
    let bounds = Bounds::new(point(px(at.x - radius), px(at.y - radius)), size(d, d));
    window.paint_quad(fill(bounds, color).corner_radii(px(radius)));
}

impl Plot for Spark {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        let rem = window.rem_size().as_f32();
        let (end_r, mark_r, ring) = (
            design_px(END_RADIUS, rem),
            design_px(MARK_RADIUS, rem),
            design_px(RING, rem),
        );
        let inset = end_r + ring;
        let rect = Rect::new(
            bounds.origin.x.as_f32(),
            bounds.origin.y.as_f32(),
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
        SCRATCH.with_borrow_mut(|scratch| {
            let marks = layout(&self.values, self.range, self.mark, rect, inset, scratch);
            // `with_content_mask` intersects with the mask in force, so the
            // glyph never paints into a neighbouring cell.
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if scratch.points.len() > 1
                    && let Some(path) = stroke_points(&scratch.points, LINE_WIDTH)
                {
                    window.paint_path(path, self.paint.line);
                }
                // A value with a gap or the series' edge on both sides has
                // no segment to show it; it reads as a dot in the line's color.
                for &p in &scratch.isolated {
                    dot(window, p, mark_r, self.paint.line);
                }
                if let Some(m) = marks.mark {
                    dot(window, m, mark_r, self.paint.mark);
                }
                if self.end_dot
                    && let Some(e) = marks.end
                {
                    dot(window, e, end_r + ring, self.paint.ring);
                    dot(window, e, end_r, self.paint.end);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, div, hsla, prelude::*, px};

    #[test]
    fn the_example_cells_are_the_exported_sizes() {
        assert_eq!(sizes::WIDTH_30D, 72.0);
        assert_eq!(sizes::HEIGHT, 16.0);
    }

    fn paint() -> SparkPaint {
        SparkPaint {
            line: hsla(0.6, 0.1, 0.6, 1.0),
            end: hsla(0.62, 0.8, 0.6, 1.0),
            mark: hsla(0.62, 0.8, 0.6, 1.0),
            ring: hsla(0.62, 0.2, 0.1, 1.0),
        }
    }

    #[test]
    fn the_builder_keeps_its_options() {
        let s = Spark::new(Arc::from(vec![1.0, 2.0]), paint())
            .range(Some((0.0, 4.0)))
            .mark(Some(1))
            .end_dot(false);
        assert_eq!(s.range, Some((0.0, 4.0)));
        assert_eq!(s.mark, Some(1));
        assert!(!s.end_dot);
        let s = Spark::new(Arc::from(vec![1.0]), paint());
        assert_eq!((s.range, s.mark, s.end_dot), (None, None, true));
    }

    struct Host {
        w: f32,
        h: f32,
        values: Arc<[f64]>,
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                div()
                    .w(px(self.w))
                    .h(px(self.h))
                    .child(Spark::new(self.values.clone(), paint()).mark(Some(2))),
            )
        }
    }

    #[gpui::test]
    fn paints_every_shape_without_panicking(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let mut host = None;
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let h = cx.new(|_| Host {
                        w: 72.0,
                        h: 16.0,
                        values: Arc::from(vec![]),
                    });
                    host = Some(h.clone());
                    cx.new(|cx| gpui_component::Root::new(h, window, cx))
                })
            })
            .unwrap();
        let host = host.unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let rising: Vec<f64> = (0..30).map(|i| 0.18 + i as f64 * 0.001).collect();
        let year: Vec<f64> = (0..252).map(|i| ((i as f64) * 0.37).sin()).collect();
        let shapes: Vec<(f32, f32, Vec<f64>)> = vec![
            (72.0, 16.0, rising.clone()),
            (72.0, 16.0, vec![]),
            (72.0, 16.0, vec![f64::NAN; 5]),
            (72.0, 16.0, vec![0.2]),
            (0.0, 0.0, rising.clone()),
            (3.0, 3.0, rising),
            (140.0, 16.0, year),
        ];
        for (w, h, values) in shapes {
            vcx.update(|window, cx| {
                host.update(cx, |host, cx| {
                    *host = Host {
                        w,
                        h,
                        values: Arc::from(values),
                    };
                    cx.notify();
                });
                let _ = window.draw(cx);
            });
            vcx.run_until_parked();
        }
    }
}
