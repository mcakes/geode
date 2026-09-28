//! The in-tile y/n confirm. While armed the prompt holds the keyboard: a
//! bare `y` or its Yes button confirms and runs the module's action; any
//! other key, its No button, a pointer press anywhere else in the tile (the
//! gap between the buttons included), or focus leaving the prompt cancels.
//! Every
//! answer blurs the prompt before its handle drops, so the keyboard goes
//! back to the shell root and the shell's restoration path returns it to
//! the tile surface. The module supplies the question and what `y` does;
//! this door owns arming, the answers and the blur.

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, Div, ElementId, Entity, FocusHandle,
    KeyDownEvent, Pixels, Point, SharedString, Subscription, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Sizable as _, Theme, h_flex};
use std::cell::RefCell;
use std::rc::Rc;

/// An armed confirm: the module's payload, the question, and the handle
/// the prompt holds the keyboard on. `_blur` is the focus-leaving answer;
/// dropping the confirm drops it, so an answered confirm never also hears
/// its own blur.
pub struct Confirm<P> {
    payload: P,
    prompt: SharedString,
    focus: FocusHandle,
    /// The window the prompt's handle lives in, for [`withdraw`], which runs
    /// where no `Window` is at hand.
    window: AnyWindowHandle,
    /// The Yes and No buttons' bounds as last painted, written by the
    /// button row's prepaint and read by [`cancel_on_press`]: a press on a
    /// button is the button's answer, not a cancel. Empty until painted,
    /// so a press before the first paint cancels.
    buttons: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    _blur: Subscription,
}

impl<P> Confirm<P> {
    pub fn payload(&self) -> &P {
        &self.payload
    }

    pub fn prompt_text(&self) -> &SharedString {
        &self.prompt
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn holds_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Whether `at` is on the Yes or No button as last painted (not in the
    /// gap between them).
    fn on_a_button(&self, at: Point<Pixels>) -> bool {
        self.buttons.borrow().iter().any(|b| b.contains(&at))
    }

    /// Blur, then drop: a focused handle dropped unblurred leaves window
    /// focus on an element no longer painted, and later keys reach no
    /// listener.
    fn disarm(self, window: &mut Window, cx: &mut App) -> P {
        if self.focus.is_focused(window) {
            window.blur(cx);
        }
        self.payload
    }
}

/// The tile a confirm lives on.
pub trait ConfirmHost: Sized + 'static {
    type Payload: 'static;
    /// The tile's one confirm slot.
    fn confirm_slot(&mut self) -> &mut Option<Confirm<Self::Payload>>;
    /// `y`: the prompt is already blurred and dropped.
    fn confirmed(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>);
    /// Any other answer: the prompt is already blurred and dropped.
    fn cancelled(&mut self, payload: Self::Payload, window: &mut Window, cx: &mut Context<Self>);
}

/// Arm a confirm asking `prompt`, focusing its prompt. A confirm already
/// armed is replaced, never stacked, and the replaced one is not answered.
pub fn arm<T: ConfirmHost>(
    host: &mut T,
    payload: T::Payload,
    prompt: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if let Some(old) = host.confirm_slot().take() {
        let _ = old.disarm(window, cx);
    }
    let focus = cx.focus_handle();
    focus.focus(window, cx);
    // Focus leaving the prompt answers no, whatever took it.
    let blur = cx.on_blur(&focus, window, |host: &mut T, window, cx| {
        cancel(host, window, cx);
    });
    *host.confirm_slot() = Some(Confirm {
        payload,
        prompt: prompt.into(),
        focus,
        window: window.window_handle(),
        buttons: Rc::default(),
        _blur: blur,
    });
}

/// The prompt's key handler. While armed every key is the confirm's
/// (answers `true`): bare `y` confirms; `n`, escape, a motion, a chord or a
/// shifted `y` cancels. A key that answers the question must not also act
/// on the tile or the shell.
pub fn key<T: ConfirmHost>(
    host: &mut T,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<T>,
) -> bool {
    let ks = &event.keystroke;
    answer(host, ks.key == "y" && !ks.modifiers.modified(), window, cx)
}

/// Answer the question: `yes` confirms, anything else cancels. The one
/// answer behind the keys and the Yes/No buttons. Answers whether a
/// confirm was armed.
pub fn answer<T: ConfirmHost>(
    host: &mut T,
    yes: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> bool {
    let Some(armed) = host.confirm_slot().take() else {
        return false;
    };
    let payload = armed.disarm(window, cx);
    if yes {
        host.confirmed(payload, window, cx);
    } else {
        host.cancelled(payload, window, cx);
    }
    true
}

/// Answer no: a pointer press, a blur, or a verb reaching the tile some
/// other way (a palette dispatch). Answers whether a confirm was armed.
pub fn cancel<T: ConfirmHost>(host: &mut T, window: &mut Window, cx: &mut Context<T>) -> bool {
    let Some(armed) = host.confirm_slot().take() else {
        return false;
    };
    let payload = armed.disarm(window, cx);
    host.cancelled(payload, window, cx);
    true
}

/// Withdraw the question where no `Window` is at hand (a delivery that
/// changed what it asked about). The blur subscription drops here, before
/// the next draw dispatches blur, so the prompt losing focus — to the
/// deferred blur, or to a question the module arms in the same update — is
/// never heard as an answer. The handle travels into the deferral, so it
/// is never dropped still focused. The module says why.
pub fn withdraw<T: ConfirmHost>(host: &mut T, cx: &mut Context<T>) -> Option<T::Payload> {
    let Confirm {
        payload,
        focus,
        window,
        _blur,
        ..
    } = host.confirm_slot().take()?;
    drop(_blur);
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            if focus.is_focused(window) {
                window.blur(cx);
            }
        });
    });
    Some(payload)
}

/// The prompt: the question on the element that holds the keyboard, and
/// its Yes and No buttons. The question's key listener runs on the focused
/// element, before the shell root's, and stops every key it answers. A
/// press on the question takes no focus (and none passes to a focusable
/// ancestor): the press has answered, and the shell's restoration path
/// returns the keyboard to the tile, as for a press anywhere else.
///
/// Yes is `y` and No any other key ([`answer`]). A left press on a button
/// moves no focus (gpui-component's `Button` prevents the default focus
/// move) and [`cancel_on_press`] lets it through, so the question stands,
/// keyboard and all, until the click lands. The press still bubbles to the
/// shell's tile listener, whose focus restore keeps the prompt because the
/// prompt holds focus. The buttons are selected as `{selector}-yes` and
/// `{selector}-no`.
pub fn prompt<T: ConfirmHost>(
    confirm: &Confirm<T::Payload>,
    tile: &Entity<T>,
    selector: impl Fn() -> String + 'static,
    theme: &Theme,
) -> Div {
    let selector = Rc::new(selector);
    let key_tile = tile.clone();
    let tile_key = tile.entity_id().as_u64();
    let button = |yes: bool| {
        let tile = tile.clone();
        let selector = selector.clone();
        let (name, label) = if yes {
            ("confirm-yes", "Yes")
        } else {
            ("confirm-no", "No")
        };
        let button = Button::new(ElementId::NamedInteger(
            SharedString::new_static(name),
            tile_key,
        ))
        .xsmall()
        .label(label)
        .on_click(move |_: &ClickEvent, window, cx| {
            tile.update(cx, |t, cx| answer(t, yes, window, cx));
        });
        div()
            .debug_selector(move || format!("{}-{}", selector(), if yes { "yes" } else { "no" }))
            .child(if yes { button.danger() } else { button.ghost() })
    };
    let question_selector = selector.clone();
    let buttons = confirm.buttons.clone();
    h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .track_focus(&confirm.focus)
                .debug_selector(move || question_selector())
                .text_color(theme.foreground)
                .child(confirm.prompt.clone())
                // A press on the question itself: `cancel_on_press` has
                // already answered no and blurred the handle, but this
                // frame's element still tracks it, and gpui's press-to-focus
                // would hand the keyboard back to a prompt that is gone.
                // Bubble listeners run before the element's own focus
                // transfer, which honours this.
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if key_tile.update(cx, |t, cx| key(t, event, window, cx)) {
                        cx.stop_propagation();
                    }
                }),
        )
        .child(
            h_flex()
                .gap_1()
                // Where the buttons are, for `cancel_on_press`: geometry
                // from this frame's layout, not state.
                .on_children_prepainted(move |bounds, _, _| {
                    let mut buttons = buttons.borrow_mut();
                    buttons.clear();
                    buttons.extend(bounds);
                })
                .child(button(true))
                .child(button(false)),
        )
}

/// Cancel on any pointer press in the tile while armed, except one on the
/// prompt's Yes or No button (the gap between them cancels). Capture phase,
/// so it runs before the press reaches what it was aimed at, and it never
/// stops the press. A press on a header or a button moves no focus, so the
/// blur answer alone would leave the question standing behind the click.
pub fn cancel_on_press<E, T>(root: E, armed: bool, tile: &Entity<T>) -> E
where
    E: InteractiveElement + FluentBuilder,
    T: ConfirmHost,
{
    let tile = tile.clone();
    root.when(armed, move |el| {
        el.capture_any_mouse_down(move |event, window, cx| {
            tile.update(cx, |t, cx| {
                let on_a_button = t
                    .confirm_slot()
                    .as_ref()
                    .is_some_and(|c| c.on_a_button(event.position));
                if !on_a_button {
                    cancel(t, window, cx);
                }
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, Render, TestAppContext, VisualTestContext, px};
    use gpui_component::{ActiveTheme as _, v_flex};

    struct Probe {
        confirm: Option<Confirm<&'static str>>,
        confirmed: Vec<&'static str>,
        cancelled: Vec<&'static str>,
        /// Keys that reached the probe's root past the prompt.
        leaked: Vec<String>,
    }

    impl ConfirmHost for Probe {
        type Payload = &'static str;
        fn confirm_slot(&mut self) -> &mut Option<Confirm<&'static str>> {
            &mut self.confirm
        }
        fn confirmed(&mut self, p: &'static str, _: &mut Window, cx: &mut Context<Self>) {
            self.confirmed.push(p);
            cx.notify();
        }
        fn cancelled(&mut self, p: &'static str, _: &mut Window, cx: &mut Context<Self>) {
            self.cancelled.push(p);
            cx.notify();
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tile = cx.entity();
            let root = v_flex()
                .size_full()
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, _| {
                    this.leaked.push(e.keystroke.key.clone())
                }))
                .child(
                    div()
                        .debug_selector(|| "probe-body".into())
                        .h(px(40.))
                        .w_full()
                        .child("body"),
                )
                .when_some(self.confirm.as_ref(), |el, c| {
                    el.child(prompt(c, &tile, || "probe-prompt".into(), cx.theme()))
                });
            cancel_on_press(root, self.confirm.is_some(), &tile)
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<Probe>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (probe, vcx) = cx.add_window_view(|_, _| Probe {
            confirm: None,
            confirmed: Vec::new(),
            cancelled: Vec::new(),
            leaked: Vec::new(),
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        (probe, vcx)
    }

    fn draw(vcx: &mut VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn arm_probe(
        probe: &Entity<Probe>,
        payload: &'static str,
        vcx: &mut VisualTestContext,
    ) -> FocusHandle {
        vcx.update(|window, cx| probe.update(cx, |p, cx| arm(p, payload, "go? (y/n)", window, cx)));
        draw(vcx);
        let focus = probe.read_with(vcx, |p, _| {
            p.confirm.as_ref().expect("armed").focus_handle().clone()
        });
        assert!(
            vcx.update(|window, _| focus.is_focused(window)),
            "the prompt holds the keyboard"
        );
        focus
    }

    /// The keyboard went back: the prompt's handle is not focused and no
    /// other handle was focused in its place (the shell's restoration path
    /// returns focus to the tile surface from here).
    fn gave_the_keyboard_back(focus: &FocusHandle, vcx: &mut VisualTestContext) {
        assert!(
            !vcx.update(|window, _| focus.is_focused(window)),
            "blurred before it dropped"
        );
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn y_confirms_once_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirmed, vec!["x"]);
            assert!(p.cancelled.is_empty());
            assert!(p.confirm.is_none());
            assert!(
                p.leaked.is_empty(),
                "the answer was the confirm's alone: {:?}",
                p.leaked
            );
        });
        gave_the_keyboard_back(&focus, vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirmed, vec!["x"], "a later y answers nothing")
        });
    }

    #[gpui::test]
    fn any_other_key_cancels_and_is_consumed(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        for key in ["n", "escape", "j", "shift-y", "ctrl-y"] {
            let focus = arm_probe(&probe, "x", vcx);
            vcx.simulate_keystrokes(key);
            draw(vcx);
            probe.read_with(vcx, |p, _| {
                assert!(p.confirmed.is_empty(), "{key} confirmed");
                assert!(p.confirm.is_none(), "{key}");
                assert!(
                    p.leaked.is_empty(),
                    "{key} reached the root: {:?}",
                    p.leaked
                );
            });
            gave_the_keyboard_back(&focus, vcx);
        }
        probe.read_with(vcx, |p, _| assert_eq!(p.cancelled.len(), 5));
    }

    #[gpui::test]
    fn a_pointer_press_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        for selector in ["probe-body", "probe-prompt"] {
            let focus = arm_probe(&probe, "x", vcx);
            let at = vcx.debug_bounds(selector).expect("painted").center();
            vcx.simulate_click(at, Modifiers::default());
            draw(vcx);
            probe.read_with(vcx, |p, _| {
                assert!(p.confirm.is_none(), "a press on {selector} cancels");
                assert!(p.confirmed.is_empty());
            });
            gave_the_keyboard_back(&focus, vcx);
        }
        probe.read_with(vcx, |p, _| assert_eq!(p.cancelled, vec!["x", "x"]));
    }

    /// Yes is `y`, No any other key: each answers once, and the press on
    /// the button did not cancel the question before the click landed.
    #[gpui::test]
    fn the_yes_button_confirms_and_the_no_button_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        let at = vcx.debug_bounds("probe-prompt-yes").expect("Yes").center();
        vcx.simulate_click(at, Modifiers::default());
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirmed, vec!["x"]);
            assert!(p.cancelled.is_empty(), "the press did not cancel first");
            assert!(p.confirm.is_none());
        });
        gave_the_keyboard_back(&focus, vcx);

        let focus = arm_probe(&probe, "z", vcx);
        let at = vcx.debug_bounds("probe-prompt-no").expect("No").center();
        vcx.simulate_click(at, Modifiers::default());
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirmed, vec!["x"]);
            assert_eq!(p.cancelled, vec!["z"], "No answered once");
        });
        gave_the_keyboard_back(&focus, vcx);
    }

    /// Between a button's press and its click the question stands and the
    /// prompt keeps the keyboard: the press moved no focus.
    #[gpui::test]
    fn a_press_on_a_button_keeps_the_question_and_the_keyboard(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        let at = vcx.debug_bounds("probe-prompt-yes").expect("Yes").center();
        vcx.simulate_mouse_down(at, gpui::MouseButton::Left, Modifiers::default());
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert!(p.confirm.is_some(), "still armed");
            assert!(p.cancelled.is_empty());
        });
        assert!(vcx.update(|window, _| focus.is_focused(window)));
        vcx.simulate_mouse_up(at, gpui::MouseButton::Left, Modifiers::default());
        draw(vcx);
        probe.read_with(vcx, |p, _| assert_eq!(p.confirmed, vec!["x"]));
    }

    /// The gap between Yes and No is not a button: a press there cancels.
    #[gpui::test]
    fn a_press_between_the_buttons_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        let yes = vcx.debug_bounds("probe-prompt-yes").expect("Yes");
        let no = vcx.debug_bounds("probe-prompt-no").expect("No");
        assert!(yes.right() < no.left(), "fixture: a gap between them");
        let gap = gpui::point((yes.right() + no.left()) / 2.0, yes.center().y);
        vcx.simulate_click(gap, Modifiers::default());
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.cancelled, vec!["x"]);
            assert!(p.confirmed.is_empty());
        });
        gave_the_keyboard_back(&focus, vcx);
    }

    #[gpui::test]
    fn a_blur_cancels(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.update(|window, cx| window.blur(cx));
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.cancelled, vec!["x"]);
            assert!(p.confirm.is_none());
        });
        gave_the_keyboard_back(&focus, vcx);
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert!(p.confirmed.is_empty(), "a later y answers nothing")
        });
    }

    #[gpui::test]
    fn withdraw_blurs_without_answering(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let focus = arm_probe(&probe, "x", vcx);
        vcx.update(|_, cx| probe.update(cx, |p, cx| assert_eq!(withdraw(p, cx), Some("x"))));
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert!(p.confirm.is_none());
            assert!(
                p.cancelled.is_empty(),
                "the withdrawn prompt's blur is not a no"
            );
            assert!(p.confirmed.is_empty());
        });
        gave_the_keyboard_back(&focus, vcx);
    }

    /// A withdrawal's blur answer is gone with it. gpui dispatches blur at
    /// the next draw, so a question armed in the same update as the
    /// withdrawal (a delivery withdrawing one confirm, the module asking
    /// another) moves focus off the withdrawn prompt at that draw; a blur
    /// answer still subscribed would answer the new question no.
    #[gpui::test]
    fn a_question_armed_behind_a_withdrawal_stands(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let first = arm_probe(&probe, "x", vcx);
        vcx.update(|window, cx| {
            probe.update(cx, |p, cx| {
                assert_eq!(withdraw(p, cx), Some("x"));
                arm(p, "z", "again? (y/n)", window, cx);
            })
        });
        draw(vcx);
        probe.read_with(vcx, |p, _| {
            assert!(
                p.cancelled.is_empty(),
                "the withdrawn prompt answered: {:?}",
                p.cancelled
            );
            assert_eq!(p.confirm.as_ref().map(|c| *c.payload()), Some("z"));
        });
        let second = probe.read_with(vcx, |p, _| {
            p.confirm.as_ref().expect("armed").focus_handle().clone()
        });
        assert!(!vcx.update(|window, _| first.is_focused(window)));
        assert!(
            vcx.update(|window, _| second.is_focused(window)),
            "the new prompt holds the keyboard"
        );
        vcx.simulate_keystrokes("y");
        draw(vcx);
        probe.read_with(vcx, |p, _| assert_eq!(p.confirmed, vec!["z"]));
        gave_the_keyboard_back(&second, vcx);
    }

    #[gpui::test]
    fn arming_again_replaces_without_answering(cx: &mut TestAppContext) {
        let (probe, vcx) = open(cx);
        let first = arm_probe(&probe, "x", vcx);
        let second = arm_probe(&probe, "z", vcx);
        probe.read_with(vcx, |p, _| {
            assert_eq!(p.confirm.as_ref().map(|c| *c.payload()), Some("z"));
            assert!(
                p.cancelled.is_empty(),
                "the replaced question is not answered"
            );
        });
        assert!(!vcx.update(|window, _| first.is_focused(window)));
        assert!(vcx.update(|window, _| second.is_focused(window)));
    }
}
