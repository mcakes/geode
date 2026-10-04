//! The `.` action menu's lifecycle and the differences axis's fixed y
//! domain. The rows are `core::menu`'s over the shared `geode_tile::menu`
//! door; a pick closes the menu and re-enters `dispatch` on the row's
//! action, so a row, its key and the palette take one path. The menu
//! opens from `.`, the header's `\u{22ef}` and a right press on the chart's
//! plot of a focused tile.

use geode_chart::core::scale::nice_outward;
use geode_chart::core::view::View;
use geode_chart::paint::y_tick_hint;
use geode_chart::{Hit, hit_test};
use geode_shell::actions::ActionId;
use geode_tile::menu::{Menu, MenuHost, Row, live_bindings};
use gpui::{App, Context, MouseButton, MouseDownEvent, Window};

use super::VolsliceTile;
use super::picker::{ActionsState, Popup};
use crate::core::build::{DIFF_AXIS, restyled};
use crate::core::menu::{self, MenuInputs, NO_DIFF_DOMAIN};

impl VolsliceTile {
    /// The view the chart paints at: the tile's, or the model's whole
    /// extent before the first model set one.
    pub(super) fn painted_view(&self) -> View {
        self.view
            .unwrap_or_else(|| View::with_min_span(self.model.full(), 0.0))
    }

    /// The differences axis's domain as painted now: its fixed limit, or
    /// the autoscaled extent of what the view shows. The chart model
    /// answers, so the frozen domain is the one the element scales over.
    pub(super) fn diff_domain(&self) -> Option<(f64, f64)> {
        self.model.side_domain(DIFF_AXIS, self.painted_view())
    }

    /// The action menu's rows over the tile as it is now.
    pub(super) fn menu_rows(&self, cx: &App) -> Vec<Row<ActionId>> {
        let loaded = self.loaded.kinds();
        menu::rows(&MenuInputs {
            state: &self.state,
            loaded: &loaded,
            following: self.frame.read(cx).following(),
            diff_domain: self.diff_domain().is_some(),
        })
    }

    /// `.` and the `\u{22ef}` button: open the action menu, or close it
    /// when it is up. Another popup up closes first.
    pub(crate) fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.popup, Some(Popup::Actions(_))) {
            self.close_popup(window, cx);
            return;
        }
        self.open_menu(window, cx);
    }

    /// Open the action menu over whatever popup is up, the highlight on
    /// its first enabled row.
    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popup(window, cx);
        let rows = self.menu_rows(cx);
        self.popup = Some(Popup::Actions(ActionsState {
            menu: Menu::new(rows, &live_bindings(cx)),
            ids: self.chrome.menu_ids.clone(),
        }));
        cx.notify();
    }

    /// `enter` in the action menu: pick the highlighted row. An
    /// all-disabled menu has no highlight, and `enter` does nothing.
    pub(super) fn pick_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Popup::Actions(a)) = &self.popup
            && let Some(i) = a.menu.highlighted()
        {
            self.menu_pick(i, window, cx);
        }
    }

    /// Rebuild an open menu's rows when what they promise moved under it
    /// (a `:` line, a delivery, a view move that changes whether the
    /// differences show). Runs from `refresh_chrome`, only while the menu
    /// is up.
    pub(super) fn refresh_menu(&mut self, cx: &App) {
        if !matches!(self.popup, Some(Popup::Actions(_))) {
            return;
        }
        let rows = self.menu_rows(cx);
        if let Some(Popup::Actions(a)) = &mut self.popup {
            a.menu.replace_rows(rows, &live_bindings(cx));
        }
    }

    /// Re-resolve the open menu's key hints against a republished keymap.
    pub(super) fn rehint_menu(&mut self, cx: &App) {
        if let Some(Popup::Actions(a)) = &mut self.popup {
            a.menu.rehint(&live_bindings(cx));
        }
    }

    /// A right press on the chart: over a plot of a focused tile it opens
    /// the action menu, as `.` does. The press that focuses the tile does
    /// nothing else, as a strip press does; the shell's right-press route
    /// only focuses, the tile answering no `press_context`.
    pub(crate) fn chart_right_pressed(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Right || !self.focused {
            return;
        }
        let rem_px = window.rem_size().as_f32();
        let Some((bounds, layout, rect)) = self.geometry(rem_px) else {
            return;
        };
        let x = (event.position.x - bounds.origin.x).as_f32();
        let y = (event.position.y - bounds.origin.y).as_f32();
        if let Hit::Plot { .. } = hit_test(&layout, rect, rem_px, x, y) {
            self.open_menu(window, cx);
        }
    }

    /// `volslice::fix_diff_y`: freeze the differences axis at the domain
    /// it shows now, widened outward to the axis's tick step so the frozen
    /// axis holds everything that was shown and its ends are tick values;
    /// or, when fixed, autoscale it again. With nothing on that axis there
    /// is nothing to freeze, and the action refuses.
    pub(super) fn fix_diff_y(&mut self, window: &Window, cx: &mut Context<Self>) {
        let next = match self.state.diff_ylim {
            Some(_) => None,
            None => match self.diff_domain() {
                Some(domain) => Some(nice_outward(domain, self.diff_tick_hint(window))),
                None => {
                    self.refuse(NO_DIFF_DOMAIN.to_string(), cx);
                    return;
                }
            },
        };
        self.set_diff_ylim(next, cx);
    }

    /// The tick count the differences axis asks for at its last painted
    /// height; five before the chart is painted.
    pub(super) fn diff_tick_hint(&self, window: &Window) -> usize {
        let rem_px = window.rem_size().as_f32();
        self.geometry(rem_px)
            .and_then(|(_, layout, _)| layout.lower)
            .map_or(5, |lower| y_tick_hint(lower.plot.h, rem_px))
    }

    /// Set the differences axis's fixed domain (`None`: autoscaled). The
    /// painted model is the same slots under a new version, which the
    /// element's chrome cache needs; a later model is built with it.
    pub(super) fn set_diff_ylim(&mut self, ylim: Option<(f64, f64)>, cx: &mut Context<Self>) {
        if self.state.diff_ylim == ylim {
            return;
        }
        self.state.diff_ylim = ylim;
        self.version += 1;
        self.model = restyled(&self.model, self.state.split, ylim, self.version);
        cx.notify();
    }

    /// The open menu's tick on `id`'s row, `None` with no menu up.
    #[cfg(test)]
    pub(crate) fn menu_tick(&self, id: &str) -> Option<Option<bool>> {
        match &self.popup {
            Some(Popup::Actions(a)) => Some(
                a.menu
                    .rows()
                    .iter()
                    .filter_map(Row::action)
                    .find(|r| r.pick().0 == id)
                    .and_then(|r| r.tick()),
            ),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn menu_open(&self) -> Option<(Vec<String>, Option<usize>)> {
        match &self.popup {
            Some(Popup::Actions(a)) => Some((
                a.menu
                    .rows()
                    .iter()
                    .filter_map(Row::action)
                    .map(|r| r.pick().0.clone())
                    .collect(),
                a.menu.highlighted(),
            )),
            _ => None,
        }
    }
}

impl MenuHost for VolsliceTile {
    /// The pointer resting on row `index`: the mouse form of `j`/`k`.
    /// Change-only: gpui fires it on every move over the row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(Popup::Actions(a)) = &mut self.popup
            && a.menu.highlight(index)
        {
            cx.notify();
        }
    }

    /// `enter` on the highlighted row, or a click on any row. A disabled
    /// row's reason becomes the notice and the menu stays; an enabled one
    /// closes the menu and dispatches its action.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Actions(a)) = &self.popup else {
            return;
        };
        match a.menu.pick(index) {
            Some(Ok(id)) => {
                self.close_popup(window, cx);
                self.dispatch(&id, None, window, cx);
            }
            Some(Err(reason)) => self.refuse(reason.to_string(), cx),
            None => {}
        }
    }
}
