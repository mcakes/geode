//! The Config section's body: the diagnostics panel on the left (the
//! current batch, or the prior batches behind the History button) beside
//! the effective-values table on the right. The right table is the page's
//! cursor table and takes the keys; the left one is pointer-driven, and
//! each panel paints its own toolbar row and detail strip.

use geode_shell::actions::ActionId;
use geode_shell::module::ShellActions;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, SharedString, WeakEntity};
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};

use crate::page::DiagnosticsPage;
use crate::page_chrome::{detail_strip, probed};
use crate::prepared::PreparedRow;
use crate::table::{SectionDelegate, table_el};

/// What the page hands the Config body to paint; the page keeps the state.
pub(crate) struct ConfigView<'a> {
    pub diag_table: &'a Entity<TableState<SectionDelegate>>,
    /// The left panel's selected row, for its detail strip.
    pub diag_row: Option<&'a PreparedRow>,
    pub history: bool,
    pub history_label: SharedString,
    pub table: &'a Entity<TableState<SectionDelegate>>,
    /// The cursor row, for the right panel's detail strip.
    pub row: Option<&'a PreparedRow>,
    pub filter: Input,
    pub actions: ShellActions,
}

pub(crate) fn render(
    view: ConfigView<'_>,
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let border = cx.theme().border;
    let (current, history) = (weak.clone(), weak);
    let left = v_flex()
        .w_1_2()
        .min_w_0()
        .min_h_0()
        .border_r_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_2()
                .p_2()
                .items_center()
                .child(probed(
                    "diagnostics-diag-current",
                    Button::new("diagnostics-diag-current")
                        .xsmall()
                        .selected(!view.history)
                        .label("Current")
                        .on_click(move |_, _window, cx| {
                            let _ = current.update(cx, |p, cx| p.set_config_history(false, cx));
                        }),
                ))
                .child(probed(
                    "diagnostics-diag-history",
                    Button::new("diagnostics-diag-history")
                        .xsmall()
                        .selected(view.history)
                        .label(view.history_label)
                        .on_click(move |_, _window, cx| {
                            let _ = history.update(cx, |p, cx| p.set_config_history(true, cx));
                        }),
                )),
        )
        .child(table_el(view.diag_table))
        .child(detail_strip(
            "diagnostics-diag-detail",
            view.diag_row,
            None,
            cx,
        ));
    let actions = view.actions;
    let right = v_flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .child(
            h_flex()
                .gap_2()
                .p_2()
                .items_center()
                .child(view.filter)
                .child(probed(
                    "diagnostics-open-config-dir",
                    Button::new("diagnostics-open-config-dir")
                        .outline()
                        .xsmall()
                        .label("Open config directory")
                        .on_click(move |_, window, cx| {
                            actions(&ActionId("config::open_directory".into()), window, cx);
                        }),
                )),
        )
        .child(table_el(view.table))
        .child(detail_strip("diagnostics-detail", view.row, None, cx));
    // `h_flex` centers its items; both panels must stretch to the body's
    // height or their tables get no height to paint rows in.
    h_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .items_stretch()
        .child(left)
        .child(right)
        .into_any_element()
}
