//! The tile's two choosers. The underlying picker is a field over the
//! diagnostics catalog's underlyings, ranked as typed; the diff chooser is
//! a fieldless list of `none` and every ordered pair of loaded kinds. Both
//! rank through `geode_shell::choice::ChoiceList`. Opening either closes the
//! other; any other tile verb closes the one open before it runs.
//!
//! The picker holds the keys in `insert` mode: its field types every bare
//! key, so it publishes no `tilelist` (the shared `j`/`k` steps would
//! swallow the letters), and `up`/`down` step it through the tile's own
//! insert bindings. The chooser has no field: it reports `mode == menu` and
//! publishes `tilelist`, so the shell's `j`/`k` and arrows step it and the
//! strip's own `j`/`k` stay out.

use geode_core::document::split_key;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::popover;
use geode_shell::shell::control;
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{
    Anchor, App, Context, ElementId, Entity, Focusable as _, KeyDownEvent, SharedString, Window,
    div,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputEvent, InputState};

use super::VolsliceTile;
use crate::core::docs::{CHAIN, CVI};
use crate::core::model::{Pair, State};

/// The picker's own key context, where `tab` and `shift-tab` are reserved
/// from `Root`'s focus cycling (`crate::init`) so the field's listener
/// completes with them instead.
pub const PICKER_CONTEXT: &str = "volslice-picker";

const NONE: &str = "none";

pub(crate) struct PickerState {
    pub(crate) input: Entity<InputState>,
    pub(crate) list: ChoiceList,
    /// The options as painted, so render clones a handle, never a string.
    pub(crate) labels: Vec<SharedString>,
}

pub(crate) struct DiffState {
    pub(crate) list: ChoiceList,
    pub(crate) labels: Vec<SharedString>,
    /// Indexed like the options: `None` for the `none` row.
    pub(crate) pairs: Vec<Option<Pair>>,
}

pub(crate) enum Popup {
    Picker(PickerState),
    Diff(DiffState),
}

fn labels_of(options: &[String]) -> Vec<SharedString> {
    options
        .iter()
        .map(|o| SharedString::from(o.clone()))
        .collect()
}

impl VolsliceTile {
    /// The catalog's underlyings: the first key part of every `cvi_params`
    /// and `option_chain` partition, sorted, once each. The chain is keyed
    /// by underlying and expiry, so its partitions name an underlying many
    /// times over.
    pub(super) fn catalog_underlyings(&self, cx: &App) -> Vec<String> {
        let d = self.diagnostics.read(cx);
        let Some(catalog) = d.catalog.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<String> = catalog
            .datasets
            .iter()
            .filter(|ds| ds.name == CVI || ds.name == CHAIN)
            .flat_map(|ds| ds.partitions.iter())
            .filter_map(|p| split_key(&p.batch).into_iter().next())
            .filter(|u| !u.is_empty())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Ask the bridge for a fresh catalog. The notify in the same update
    /// is what wakes the bridge's drain: `request_catalog` bumps no version.
    fn request_catalog(&self, cx: &mut Context<Self>) {
        self.diagnostics.update(cx, |d, cx| {
            d.request_catalog();
            cx.notify();
        });
    }

    /// The diagnostics observer's picker half: swap in a changed option
    /// list, keeping the highlight by text. `true` when it changed.
    pub(super) fn refresh_picker(&mut self, cx: &App) -> bool {
        if !matches!(self.popup, Some(Popup::Picker(_))) {
            return false;
        }
        let all = self.catalog_underlyings(cx);
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return false;
        };
        if p.list.options() == all.as_slice() {
            return false;
        }
        p.labels = labels_of(&all);
        p.list.replace_options(all);
        true
    }

    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popup(window, cx);
        self.request_catalog(cx);
        let options = self.catalog_underlyings(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("underlying"));
        // The live path while a trader types. `commit_picker` re-ranks from
        // the field's own text too: `set_value` emits no `Change`.
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                if let Some(Popup::Picker(p)) = &mut this.popup {
                    p.list.set_query(&query);
                }
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.popup = Some(Popup::Picker(PickerState {
            input,
            labels: labels_of(&options),
            list: ChoiceList::new(options, DEFAULT_CAP),
        }));
        cx.notify();
    }

    pub(super) fn open_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popup(window, cx);
        let mut pairs = vec![None];
        pairs.extend(State::pairs(&self.loaded).into_iter().map(Some));
        let options: Vec<String> = pairs
            .iter()
            .map(|p| p.map_or_else(|| NONE.to_string(), |p| p.label()))
            .collect();
        let current = self.state.diff.map(|p| p.label());
        let mut list = ChoiceList::new(options.clone(), DEFAULT_CAP);
        list.place(current.as_deref());
        self.popup = Some(Popup::Diff(DiffState {
            list,
            labels: labels_of(&options),
            pairs,
        }));
        cx.notify();
    }

    /// Close whichever popup is open. The picker's field is blurred first
    /// when it holds the keyboard: a focused handle dropped unblurred leaves
    /// the window with no focus the shell can route keys through.
    pub(super) fn close_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        if let Popup::Picker(p) = &popup
            && p.input.read(cx).focus_handle(cx).is_focused(window)
        {
            window.blur(cx);
        }
        cx.notify();
    }

    /// Step the open list by one row, clamped.
    pub(super) fn step_popup(&mut self, delta: i64, cx: &mut Context<Self>) -> bool {
        let list = match &mut self.popup {
            Some(Popup::Picker(p)) => &mut p.list,
            Some(Popup::Diff(d)) => &mut d.list,
            None => return false,
        };
        list.nav_clamped(NavCommand::Move(delta));
        cx.notify();
        true
    }

    /// `enter`: the picker sets the highlighted underlying and asks again;
    /// the chooser sets the highlighted pair and resubmits. A picker with
    /// nothing ranked stays open.
    pub(super) fn commit_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &mut self.popup {
            Some(Popup::Picker(p)) => {
                let query = p.input.read(cx).value().to_string();
                p.list.set_query(&query);
                let Some(u) = p.list.highlighted_text().map(str::to_string) else {
                    return;
                };
                self.close_popup(window, cx);
                self.set_underlying(u, cx);
            }
            Some(Popup::Diff(d)) => {
                let Some(i) = d.list.pick() else {
                    return;
                };
                let pair = d.pairs[i];
                self.close_popup(window, cx);
                if self.state.diff != pair {
                    self.state.diff = pair;
                    self.resubmit(cx);
                }
            }
            None => {}
        }
    }

    /// A painted row pressed: highlight it, then commit as `enter` would.
    pub(super) fn popup_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let list = match &mut self.popup {
            Some(Popup::Picker(p)) => &mut p.list,
            Some(Popup::Diff(d)) => &mut d.list,
            None => return,
        };
        if list.set_highlighted(row) {
            self.commit_popup(window, cx);
        }
    }

    /// The pointer form of `up`/`down`: change-only.
    pub(super) fn popup_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let list = match &mut self.popup {
            Some(Popup::Picker(p)) => &mut p.list,
            Some(Popup::Diff(d)) => &mut d.list,
            None => return,
        };
        if list.highlighted() != row && list.set_highlighted(row) {
            cx.notify();
        }
    }

    /// `tab` in the picker's field: the highlighted underlying becomes the
    /// text. `true` when the key was the picker's.
    fn picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.key != "tab" {
            return false;
        }
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return false;
        };
        if p.list.complete() {
            let text = p.list.query().to_string();
            let input = p.input.clone();
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            cx.notify();
        }
        true
    }

    /// The picker field's text and the query its list is ranked by.
    #[cfg(test)]
    pub(crate) fn picker_text(&self, cx: &App) -> Option<(String, String)> {
        match &self.popup {
            Some(Popup::Picker(p)) => Some((
                p.input.read(cx).value().to_string(),
                p.list.query().to_string(),
            )),
            _ => None,
        }
    }

    /// The open popup's rows and highlighted row, as painted.
    #[cfg(test)]
    pub(crate) fn chooser_rows(&self) -> Option<(Vec<String>, usize)> {
        let (list, labels) = match &self.popup {
            Some(Popup::Picker(p)) => (&p.list, &p.labels),
            Some(Popup::Diff(d)) => (&d.list, &d.labels),
            None => return None,
        };
        Some((
            list.painted()
                .iter()
                .map(|r| labels[r.row].to_string())
                .collect(),
            list.highlighted(),
        ))
    }
}

/// Paint the open popup, hung from the header's right edge. Rows are the
/// list's painted window, labelled from the prepared strings.
pub(crate) fn render_popup(
    popup: &Popup,
    tile: &Entity<VolsliceTile>,
    tile_id: u64,
    cx: &App,
) -> gpui::Deferred {
    let theme = cx.theme();
    let hover = control::paint(
        theme,
        control::Rest::Bare,
        theme.popover,
        theme.popover_foreground,
    )
    .hover;
    let (list, labels, kind) = match popup {
        Popup::Picker(p) => (&p.list, &p.labels, "picker"),
        Popup::Diff(d) => (&d.list, &d.labels, "diff"),
    };
    let mut surface = popover::surface(cx)
        .id(ElementId::Name(SharedString::new_static(match popup {
            Popup::Picker(_) => "volslice-picker",
            Popup::Diff(_) => "volslice-diff",
        })))
        .debug_selector(move || format!("volslice-{kind}-{tile_id}"))
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup(window, cx))
        });
    if let Popup::Picker(p) = popup {
        surface = surface.child(
            div()
                .w_full()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .key_context(PICKER_CONTEXT)
                .on_key_down({
                    let tile = tile.clone();
                    move |event: &KeyDownEvent, window, cx| {
                        if tile.update(cx, |t, cx| t.picker_key(event, window, cx)) {
                            cx.stop_propagation();
                        }
                    }
                })
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    }
    if list.painted_len() == 0 {
        surface = surface.child(popover::empty_row(theme, "no underlyings known"));
    }
    let lit = list.highlighted();
    for (i, r) in list.painted().iter().enumerate() {
        let label = labels[r.row].clone();
        let selector = label.clone();
        surface = surface.child(
            popover::row_shell(
                theme,
                hover,
                ElementId::Name(label.clone()),
                i == lit,
                move || format!("volslice-{kind}-row-{tile_id}-{selector}"),
                {
                    let tile = tile.clone();
                    move |window, cx| tile.update(cx, |t, cx| t.popup_pick(i, window, cx))
                },
            )
            .on_mouse_move({
                let tile = tile.clone();
                move |_, _, cx| tile.update(cx, |t, cx| t.popup_hover(i, cx))
            })
            .child(label),
        );
    }
    popover::anchor_popup(surface, Anchor::TopRight)
}
