//! Guide chrome uses the shell's header, keycaps, tooltips and tile menus.

use super::{GuideTile, MenuKind, sections};
use crate::document::section_for;
use geode_shell::shell::kbd;
use geode_shell::tips;
use geode_tile::header::{self, Cluster, MenuTrigger};
use geode_tile::menu;
use gpui::prelude::*;
use gpui::{Anchor, Context, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::text::TextView;
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Selectable as _, Sizable as _, h_flex,
    v_flex,
};
use std::rc::Rc;

impl GuideTile {
    fn control_label(&self, label: &'static str, action: &'static str) -> gpui::Div {
        h_flex().items_center().gap_1().child(label).when_some(
            self.hints.iter().find(|(id, _)| *id == action),
            |el, (_, keys)| el.child(kbd::binding(keys)),
        )
    }
}

impl Render for GuideTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut cluster = Cluster::new(self.id);
        cluster.close = self.close.clone();
        let entity = cx.entity();
        cluster.menu = Some(MenuTrigger {
            id: ("guide-menu", self.id.0).into(),
            selector: "guide-menu".into(),
            tip_selector: "tip-guide-menu".into(),
            action: "guide::menu",
            open: self.menu_is(MenuKind::Actions),
            on_press: Rc::new(move |_, cx| entity.update(cx, |this, cx| this.toggle_actions(cx))),
        });
        cluster.notices.extend(self.notice.clone());
        let marker = self
            .stack
            .as_ref()
            .and_then(|stack| stack.marker(cx.theme(), self.id));
        let weak = cx.entity().downgrade();
        let scroll_owner = weak.clone();
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .text_color(cx.theme().foreground)
            .bg(cx.theme().background)
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(header::frame(
                        marker,
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(div().flex_shrink_0().child("User guide"))
                            .child("/")
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(cx.theme().foreground)
                                    .child(sections()[self.section].title.clone()),
                            ),
                        cluster,
                        cx.theme(),
                    ))
                    .when_some(
                        self.menu.as_ref().filter(|m| m.kind == MenuKind::Actions),
                        |el, open| {
                            el.child(div().absolute().right_0().top_full().child(
                                menu::render_menu(
                                    &open.menu,
                                    &self.actions_ids,
                                    Anchor::TopRight,
                                    &cx.entity(),
                                    |this, _, cx| {
                                        this.menu = None;
                                        cx.notify();
                                    },
                                    cx,
                                ),
                            ))
                        },
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .flex_shrink_0()
                    .debug_selector(|| "guide-toolbar".into())
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .relative()
                            .child(
                                Button::new("guide-contents")
                                    .accessibility_label("Contents")
                                    .debug_selector(|| "guide-contents".into())
                                    .child(self.control_label("Contents", "guide::contents"))
                                    .map(|mut button| {
                                        button.interactivity().tooltip(tips::tip(
                                            "tip-guide-contents",
                                            "Guide contents",
                                            Some("guide::contents"),
                                            None,
                                        ));
                                        button
                                    })
                                    .small()
                                    .ghost()
                                    .selected(self.menu_is(MenuKind::Contents))
                                    // Toggle before the popup's outside-press handler;
                                    // waiting for click would reopen what it just closed.
                                    .capture_any_mouse_down(cx.listener(
                                        |this, event: &gpui::MouseDownEvent, _, cx| {
                                            if event.button == gpui::MouseButton::Left {
                                                this.toggle_contents(cx);
                                            }
                                        },
                                    ))
                                    .on_click(cx.listener(
                                        |this, event: &gpui::ClickEvent, _, cx| {
                                            if event.is_keyboard() {
                                                this.toggle_contents(cx);
                                            }
                                        },
                                    )),
                            )
                            .when_some(
                                self.menu.as_ref().filter(|m| m.kind == MenuKind::Contents),
                                |d, open| {
                                    d.child(div().absolute().left_0().top_full().child(
                                        menu::render_menu(
                                            &open.menu,
                                            &self.menu_ids,
                                            Anchor::TopLeft,
                                            &cx.entity(),
                                            |this, _, cx| {
                                                this.menu = None;
                                                cx.notify();
                                            },
                                            cx,
                                        ),
                                    ))
                                },
                            ),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        Button::new("guide-previous")
                            .accessibility_label("Previous section")
                            .debug_selector(|| "guide-previous".into())
                            .child(self.control_label("Previous", "guide::previous"))
                            .map(|mut button| {
                                button.interactivity().tooltip(tips::tip(
                                    "tip-guide-previous",
                                    "Previous guide section",
                                    Some("guide::previous"),
                                    None,
                                ));
                                button
                            })
                            .small()
                            .ghost()
                            .disabled(self.section == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_section(this.section.saturating_sub(1), cx)
                            })),
                    )
                    .child(
                        Button::new("guide-next")
                            .accessibility_label("Next section")
                            .debug_selector(|| "guide-next".into())
                            .child(self.control_label("Next", "guide::next"))
                            .map(|mut button| {
                                button.interactivity().tooltip(tips::tip(
                                    "tip-guide-next",
                                    "Next guide section",
                                    Some("guide::next"),
                                    None,
                                ));
                                button
                            })
                            .small()
                            .ghost()
                            .disabled(self.section + 1 == sections().len())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_section(this.section + 1, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .on_prepaint(move |_, _, cx| {
                        let owner = scroll_owner.clone();
                        let ready = owner
                            .read_with(cx, |this, cx| {
                                this.pending_scroll.is_some()
                                    && this.text.read(cx).list_state().item_count()
                                        == this.expected_blocks
                            })
                            .unwrap_or(false);
                        if ready {
                            // The component resets its virtual list after parsing.
                            // Apply the prepared target after that layout, never to
                            // the previous section's list or from the render method.
                            cx.defer(move |cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    if this.text.read(cx).list_state().item_count()
                                        == this.expected_blocks
                                        && let Some(offset) = this.pending_scroll.take()
                                    {
                                        this.text.read(cx).list_state().scroll_to(offset);
                                        cx.notify();
                                    }
                                });
                            });
                        }
                    })
                    .child(
                        TextView::new(&self.text)
                            .scrollable(true)
                            .size_full()
                            .p_4()
                            .on_link_click(move |url, _, _, cx| {
                                if let Some(section) = url.strip_prefix('#').and_then(section_for) {
                                    let _ =
                                        weak.update(cx, |this, cx| this.show_section(section, cx));
                                }
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .debug_selector(|| "guide-footer".into())
                    .child(self.control_label("Find", "tile::find"))
                    .child(self.control_label("Actions", "guide::menu"))
                    .when(self.query.is_some(), |el| {
                        el.child(
                            div()
                                .child(self.search_status.clone())
                                .debug_selector(|| "guide-search-status".into()),
                        )
                        .child(self.control_label("Next match", "guide::next_match"))
                        .child(self.control_label("Previous match", "guide::previous_match"))
                        .child(self.control_label("Clear", "guide::clear_find"))
                    }),
            )
    }
}
