//! The cursor and the filter: `h j k l` walk the graph's edges and
//! layers (`core::layout::neighbour`), `g 1`…`g 9` jump to a card by its
//! layout order (`nth`), and `/` opens a field in the footer whose text
//! dims the cards that fail a fuzzy match as it is typed.
//!
//! The filter follows the shell's dialog-filter rule: `escape` restores
//! the filter in force when `/` was pressed and `enter` keeps what was
//! typed, landing the cursor on the first lit card in layout order. The
//! field holds the keys in `insert` mode, so every bare key types; only
//! `enter` and `escape` are the tile's there. While the field is up its
//! text is the filter, so the graph previews the match live.
//!
//! Every cursor move writes the session key and rebuilds the model (the
//! cursor's card and its edges take their tones), then scrolls the card
//! into view. The viewport is what paint last measured.

use geode_core::calibration::StageName;
use gpui::{AppContext as _, Context, Entity, Focusable as _, Window};
use gpui_component::input::{InputEvent, InputState};

use super::CalibrationTile;
use crate::core::layout::{Move, neighbour, nth};

/// The filter field while it is up.
pub(super) struct FilterField {
    pub(super) input: Entity<InputState>,
    /// The filter in force when `/` was pressed: what `escape` restores.
    entry: Option<String>,
}

/// The viewport the layout was last placed in, in window pixels, and the
/// rem it was placed at. The defaults stand until paint measures the body;
/// the first measurement rebuilds the model at the real size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Viewport {
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) rem: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            width: 1200.0,
            height: 600.0,
            rem: 16.0,
        }
    }
}

impl CalibrationTile {
    /// A `Move` from the cursor; with no cursor, the first card in layout
    /// order. A step that leaves the graph leaves the cursor where it is.
    pub(super) fn move_cursor(&mut self, motion: Move, cx: &mut Context<Self>) {
        let Some(model) = self.model.as_ref() else {
            return;
        };
        // The pipeline the model was built from, held or live: the cursor
        // walks what is painted.
        let Some(pipeline) = self.graph_pipeline() else {
            return;
        };
        let next = match &self.cursor {
            Some(from) => neighbour(&model.layout, &pipeline, from, motion),
            None => nth(&model.layout, 1),
        };
        if let Some(next) = next {
            self.set_cursor(Some(next), cx);
        }
    }

    /// `g N`: the Nth card in layout order; past the last card, nothing.
    pub(super) fn jump(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(next) = self.model.as_ref().and_then(|m| nth(&m.layout, n)) else {
            return;
        };
        self.set_cursor(Some(next), cx);
    }

    /// Set the cursor, rebuild the model and bring the card into view.
    pub(super) fn set_cursor(&mut self, cursor: Option<StageName>, cx: &mut Context<Self>) {
        if self.cursor == cursor {
            return;
        }
        self.cursor = cursor;
        self.rebuild_model(cx);
        self.scroll_cursor_into_view();
        cx.notify();
    }

    /// Paint measured the body: when its size or rem moved, the layout is
    /// placed again in the new viewport and the cursor's card kept in view.
    /// Called after the draw that measured it, never during one.
    pub(super) fn viewport_measured(&mut self, viewport: Viewport, cx: &mut Context<Self>) {
        if self.viewport == viewport {
            return;
        }
        self.viewport = viewport;
        self.rebuild_model(cx);
        self.scroll_cursor_into_view();
        cx.notify();
    }

    /// `:layout`: the graph is placed again from the home scroll. The
    /// scroll stays home even with the cursor off screen: the trader asked
    /// for the whole graph's first view, not the cursor's.
    pub(super) fn relayout(&mut self, cx: &mut Context<Self>) {
        self.scroll = (0.0, 0.0);
        self.rebuild_model(cx);
        cx.notify();
    }

    /// Move the scroll the least that puts the cursor's card wholly in
    /// view, then clamp both axes to the content.
    fn scroll_cursor_into_view(&mut self) {
        let Some(model) = self.model.as_ref() else {
            return;
        };
        let (mut sx, mut sy) = self.scroll;
        let layout = &model.layout;
        if let Some(card) = self
            .cursor
            .as_deref()
            .and_then(|c| layout.cards.iter().find(|k| k.stage == c))
        {
            sx = fit(sx, card.x, layout.card_w, self.viewport.width);
            // The caption band above the first row scrolls with the rows;
            // the cursor's card need not pull it back into view.
            sy = fit(sy, card.y, layout.card_h, self.viewport.height);
        }
        let max_x = (layout.content_w - self.viewport.width).max(0.0);
        let max_y = (layout.content_h - self.viewport.height).max(0.0);
        self.scroll = (sx.clamp(0.0, max_x), sy.clamp(0.0, max_y));
    }

    /// `/`: open the filter field over the footer, prefilled with the
    /// filter in force. A field already up keeps its text.
    pub(super) fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_field.is_some() {
            return;
        }
        let entry = self.filter.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("filter stages"));
        if let Some(text) = &entry {
            let text = text.clone();
            input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        // The live path: the field's text is the filter as it is typed, so
        // the cards dim with it.
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                this.set_filter((!text.is_empty()).then_some(text), cx);
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.filter_field = Some(FilterField { input, entry });
        cx.notify();
    }

    /// `enter` in the field: keep the typed filter, close, and land the
    /// cursor on the first lit card in layout order (none lit: it stays).
    pub(super) fn commit_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.filter_field.take() else {
            return;
        };
        let text = field.input.read(cx).value().to_string();
        self.close_field(&field, window, cx);
        self.set_filter((!text.is_empty()).then_some(text), cx);
        let first_lit = self
            .model
            .as_ref()
            .and_then(|m| m.cards.iter().find(|c| !c.dimmed))
            .map(|c| c.stage.clone());
        if first_lit.is_some() {
            self.set_cursor(first_lit, cx);
        }
        cx.notify();
    }

    /// `escape` in the field: the filter in force when `/` was pressed is
    /// restored and the field closes. `true` when a field was up.
    pub(super) fn cancel_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.filter_field.take() else {
            return false;
        };
        self.close_field(&field, window, cx);
        let entry = field.entry.clone();
        self.set_filter(entry, cx);
        cx.notify();
        true
    }

    /// Blur the field first when it holds the keyboard: a focused handle
    /// dropped unblurred leaves the window with no focus the shell can
    /// route keys through.
    fn close_field(&self, field: &FilterField, window: &mut Window, cx: &mut Context<Self>) {
        if field.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
    }

    fn set_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.rebuild_model(cx);
        cx.notify();
    }

    /// Whether the filter field or an armed question holds window focus:
    /// the shell's insert branch routes bare keys to it only while one
    /// does.
    pub fn holds_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.filter_field
            .as_ref()
            .is_some_and(|f| f.input.read(cx).focus_handle(cx).is_focused(window))
            || self.confirm.as_ref().is_some_and(|c| c.holds_focus(window))
    }
}

/// The scroll along one axis that keeps `[at, at + size)` within a
/// viewport of `span` starting at `scroll`, moving the least: unchanged
/// when the span already holds it.
fn fit(scroll: f32, at: f32, size: f32, span: f32) -> f32 {
    if at < scroll {
        at
    } else if at + size > scroll + span {
        at + size - span
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_moves_the_scroll_the_least_that_shows_the_span() {
        // In view: unchanged.
        assert_eq!(fit(100.0, 150.0, 90.0, 400.0), 100.0);
        // Past the right edge: the card's right edge meets the viewport's.
        assert_eq!(fit(0.0, 550.0, 90.0, 400.0), 240.0);
        // Before the left edge: the card's left edge meets the viewport's.
        assert_eq!(fit(300.0, 110.0, 90.0, 400.0), 110.0);
        // Wider than the viewport: the left edge wins.
        assert_eq!(fit(0.0, 50.0, 500.0, 400.0), 150.0);
    }
}
