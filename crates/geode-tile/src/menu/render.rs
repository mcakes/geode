//! The one menu painter. It reads the prepared [`Menu`] and formats
//! nothing but the lazy debug selectors.

use super::{
    Menu, MenuPick, Row, Trailing,
    paint::{MenuPaint, row_paint},
};
use crate::popover::{self, ROW_HEIGHT, ROW_INSET};
use geode_shell::shell::control::PointerStates as _;
use geode_shell::shell::{kbd, scale};
use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Context, Deferred, ElementId, Entity, MouseButton, SharedString,
    Window, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex};

/// The leading tick slot on a checked row: the same width ticked or not, so
/// a group's titles share one leading edge. Pickers that tick their
/// current row use it too.
pub const TICK_SLOT: f32 = 14.0;

/// The tile a menu is painted for: where a row press and a row hover go.
pub trait MenuHost: Sized + 'static {
    /// A row press, and the module's `enter`: one path.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>);
    /// The pointer resting on row `index`: the mouse form of stepping.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>);
}

/// A menu's element names, prepared once: the container's selector and id,
/// and the row selector prefix (row `i` is `"{row}-{i}"`).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuIds {
    menu: SharedString,
    row: SharedString,
}

impl MenuIds {
    pub fn new(menu: impl Into<SharedString>, row: impl Into<SharedString>) -> Self {
        Self {
            menu: menu.into(),
            row: row.into(),
        }
    }
}

/// Paint `menu` hung by its `corner` from the point the caller paints this
/// at. A row press picks through [`MenuHost::menu_pick`] and stops there: a
/// pick must not also reach the tile beneath (which would, say, cancel its
/// editor). A press outside runs `on_outside`. Hover moves the highlight.
pub fn render_menu<P: MenuPick, T: MenuHost>(
    menu: &Menu<P>,
    ids: &MenuIds,
    corner: Anchor,
    tile: &Entity<T>,
    on_outside: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let paint = MenuPaint::derive(theme);
    let menu_selector = ids.menu.clone();
    let mut list = popover::surface(cx)
        .id(ElementId::Name(ids.menu.clone()))
        .debug_selector(move || menu_selector.to_string())
        // Occlude what the menu covers so its hover and presses do not also reach it.
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| on_outside(t, window, cx))
        });
    for (i, row) in menu.rows().iter().enumerate() {
        list = list.child(match row {
            // gpui-component's `PopupMenu` separator: a rule bleeding into the
            // surface's inset, half a step of air either side; two pixels so
            // it reads at every rem size.
            Row::Separator => div()
                .my_0p5()
                .mx_neg_1()
                .border_b(px(2.))
                .border_color(theme.border)
                .into_any_element(),
            Row::Section(title) => div()
                .px(scale::design(ROW_INSET))
                .pt_1()
                .text_xs()
                .text_color(paint.muted)
                .overflow_hidden()
                .text_ellipsis()
                .child(title.clone())
                .into_any_element(),
            Row::Action(action) => {
                let rp = row_paint(&paint, menu.highlighted() == Some(i), action.is_enabled());
                // Two static strings: a frame formats nothing here.
                let tick: Option<&'static str> =
                    action.tick().map(|on| if on { "\u{2713}" } else { "" });
                let trailing: AnyElement = match action.trailing() {
                    Trailing::Keys(keys) => kbd::menu_binding(keys, rp.lane).into_any_element(),
                    Trailing::Text(text) => div().child(text.clone()).into_any_element(),
                    Trailing::None => div().into_any_element(),
                };
                let row_selector = ids.row.clone();
                h_flex()
                    .id(ElementId::Name(action.name().clone()))
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .when_some(rp.fill, |d, fill| d.bg(fill))
                    .text_color(rp.text)
                    .when_some(rp.pointer, |d, states| d.pointer_states(states))
                    .debug_selector(move || format!("{row_selector}-{i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.menu_hover(i, cx))
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(tick, |d, tick| {
                                d.child(
                                    div()
                                        .w(scale::design(TICK_SLOT))
                                        .flex_shrink_0()
                                        .child(tick),
                                )
                            })
                            .child(action.title().clone()),
                    )
                    .child(div().text_color(rp.lane).child(trailing))
                    .into_any_element()
            }
        });
    }
    popover::anchor_popup(list, corner)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Id, bindings};
    use super::super::*;
    use super::*;
    use geode_shell::tips::Chords;
    use gpui::{Modifiers, Point, Render, TestAppContext, VisualTestContext, px};
    use std::sync::Arc;

    fn rows() -> Vec<Row<Id>> {
        vec![
            Row::Action(ActionRow::new(Id("a"), "Alpha").hint(Hint::chord("demo::alpha"))),
            Row::Separator,
            Row::Action(ActionRow::new(Id("b"), "Beta").enabled(Err("not now".into()))),
            Row::Action(ActionRow::new(Id("c"), "Gamma")),
        ]
    }

    const REBOUND: &str =
        "[[bindings]]\n[bindings.keys]\n\"a\" = \"none\"\n\"shift+a\" = \"demo::alpha\"\n";

    fn alpha_lane(menu: &Menu<Id>) -> Lane {
        menu.rows()[0].action().unwrap().lane().clone()
    }

    fn keys(spec: &str) -> Vec<geode_shell::keymap::Keystroke> {
        geode_shell::keymap::parse_binding(spec, geode_shell::keymap::Modifiers::NONE).unwrap()
    }

    #[gpui::test]
    fn a_hint_follows_a_user_rebind_through_the_live_keymap(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(Some(REBOUND))))));
        let menu = cx.update(|cx| Menu::new(rows(), &live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("shift+a")));
    }

    #[gpui::test]
    fn rehint_follows_a_republished_keymap(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(None)))));
        let mut menu = cx.update(|cx| Menu::new(rows(), &live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("a")));
        cx.update(|cx| cx.set_global(Chords(Arc::new(bindings(Some(REBOUND))))));
        cx.update(|cx| menu.rehint(&live_bindings(cx)));
        assert_eq!(alpha_lane(&menu), Lane::Keys(keys("shift+a")));
    }

    struct Probe {
        menu: Menu<Id>,
        ids: MenuIds,
        picks: Vec<Result<Id, SharedString>>,
        outside: usize,
        under: usize,
    }

    impl MenuHost for Probe {
        fn menu_pick(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
            if let Some(p) = self.menu.pick(index) {
                self.picks.push(p);
            }
            cx.notify();
        }

        fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
            if self.menu.highlight(index) {
                cx.notify();
            }
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tile = cx.entity();
            div()
                .size_full()
                .child(
                    div()
                        .id("under")
                        .absolute()
                        .size_full()
                        .on_mouse_move(cx.listener(|this, _, _, cx| {
                            this.under += 1;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .left(px(10.))
                        .child(render_menu(
                            &self.menu,
                            &self.ids,
                            Anchor::TopLeft,
                            &tile,
                            |t: &mut Probe, _, cx| {
                                t.outside += 1;
                                cx.notify();
                            },
                            cx,
                        )),
                )
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<Probe>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (probe, vcx) = cx.add_window_view(|_, _| Probe {
            menu: Menu::new(rows(), &[]),
            ids: MenuIds::new("probe-menu", "probe-menu-row"),
            picks: Vec::new(),
            outside: 0,
            under: 0,
        });
        vcx.run_until_parked();
        (probe, vcx)
    }

    fn centre_of(vcx: &mut VisualTestContext, selector: &str) -> Point<gpui::Pixels> {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    #[gpui::test]
    fn a_hover_lights_its_row_and_the_menu_occludes_what_it_covers(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let at = centre_of(vcx, "probe-menu-row-3");
        vcx.simulate_mouse_move(at, None, Modifiers::default());
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.menu.highlighted(), Some(3));
            assert_eq!(p.under, 0, "the covered element heard no move");
        });
        let beside = centre_of(vcx, "probe-menu") + gpui::point(px(600.), px(0.));
        vcx.simulate_mouse_move(beside, None, Modifiers::default());
        probe.read_with(vcx, |p, _| {
            assert!(p.under > 0, "fixture: the counter hears moves")
        });
    }

    #[gpui::test]
    fn a_row_press_picks_a_disabled_one_gives_its_reason_and_outside_closes(
        cx: &mut TestAppContext,
    ) {
        let (probe, vcx) = open(cx);
        let row = centre_of(vcx, "probe-menu-row-0");
        vcx.simulate_click(row, Modifiers::default());
        let off = centre_of(vcx, "probe-menu-row-2");
        vcx.simulate_click(off, Modifiers::default());
        let beside = centre_of(vcx, "probe-menu") + gpui::point(px(600.), px(0.));
        vcx.simulate_click(beside, Modifiers::default());
        probe.read_with(vcx, |p, _| {
            assert_eq!(
                p.picks,
                vec![Ok(Id("a")), Err(SharedString::from("not now"))]
            );
            assert_eq!(p.outside, 1);
        });
    }
}
