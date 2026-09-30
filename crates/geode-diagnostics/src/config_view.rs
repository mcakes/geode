//! Configuration views share one full-width table region and keyboard cursor.
use crate::page::DiagnosticsPage;
use crate::page_chrome::probed;
use crate::table::{SectionDelegate, table_el};
use geode_shell::actions::ActionId;
use geode_shell::module::ShellActions;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, SharedString, WeakEntity, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};

pub(crate) struct ConfigView<'a> {
    pub focus: &'a FocusHandle,
    pub diag_table: &'a Entity<TableState<SectionDelegate>>,
    pub history: bool,
    pub values: bool,
    pub history_label: SharedString,
    pub table: &'a Entity<TableState<SectionDelegate>>,
    pub filter: Input,
    pub actions: ShellActions,
}

pub(crate) fn render(
    view: ConfigView<'_>,
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let (current, history, values) = (weak.clone(), weak.clone(), weak);
    let actions = view.actions;
    v_flex()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(probed(
                    "diagnostics-diag-current",
                    Button::new("diagnostics-diag-current")
                        .ghost()
                        .small()
                        .selected(!view.values && !view.history)
                        .label("Current issues")
                        .on_click(move |_, window, cx| {
                            let _ = current.update(cx, |p, cx| {
                                p.set_config_history(false, cx);
                                p.focus_handle().focus(window, cx);
                            });
                        }),
                ))
                .child(probed(
                    "diagnostics-diag-history",
                    Button::new("diagnostics-diag-history")
                        .ghost()
                        .small()
                        .selected(!view.values && view.history)
                        .label(view.history_label)
                        .on_click(move |_, window, cx| {
                            let _ = history.update(cx, |p, cx| {
                                p.set_config_history(true, cx);
                                p.focus_handle().focus(window, cx);
                            });
                        }),
                ))
                .child(probed(
                    "diagnostics-config-values",
                    Button::new("diagnostics-config-values")
                        .ghost()
                        .small()
                        .selected(view.values)
                        .label("Effective values")
                        .on_click(move |_, window, cx| {
                            let _ = values.update(cx, |p, cx| {
                                p.set_config_values(cx);
                                p.focus_handle().focus(window, cx);
                            });
                        }),
                )),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_2()
                .child(view.filter)
                .child(div().flex_1())
                .child(probed(
                    "diagnostics-open-config-dir",
                    Button::new("diagnostics-open-config-dir")
                        .ghost()
                        .small()
                        .label("Open config directory")
                        .on_click(move |_, window, cx| {
                            actions(&ActionId("config::open_directory".into()), window, cx);
                        }),
                )),
        )
        .child(table_el(
            if view.values {
                view.table
            } else {
                view.diag_table
            },
            view.focus,
        ))
        .into_any_element()
}
