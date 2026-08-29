//! The shell's window root view (spec §3): a single view owning the whole
//! window contents, key dispatch, and workspace state. Renders the tiling
//! tree (Task 3) as themed, absolutely-positioned tiles, with a fixed-height
//! status bar (Task 4, `status::status_bar`) below the tile area. Task 6
//! wires the real command palette.

pub mod keys;
pub mod status;

pub use keys::convert_keystroke;

use gpui::prelude::*;
use gpui::{Context, FocusHandle, KeyDownEvent, MouseButton, Window, div, px};
use gpui_component::{ActiveTheme as _, Root, v_flex};

use crate::actions::ActionRegistry;
use crate::keymap::{KeyContext, Keymap, MatchResult, Matcher, Modifiers};
use crate::tiling::{Rect, Workspaces, apply_workspace_action};
use geode_core::config::Config;

/// Everything the shell needs to run a window, assembled once by the app
/// from loaded config, the action registry, the compiled keymap, and the
/// initial workspace state (spec §3, §8). `ShellView` owns this for the
/// life of the window.
pub struct ShellServices {
    pub config: Config,
    pub registry: ActionRegistry,
    pub keymap: Keymap,
    pub mod_alias: Modifiers,
    pub workspaces: Workspaces,
}

/// The window's root view. Intercepts all keyboard input via `on_key_down`
/// rather than gpui's own action-dispatch system, because key resolution
/// here goes through the shell's own layered, sequence-aware [`Matcher`]
/// (spec §3.4), not a static `KeyBinding` table.
pub struct ShellView {
    services: ShellServices,
    matcher: Matcher,
    focus_handle: FocusHandle,
    /// Whether the command palette is open. Always false until Task 6 wires
    /// the real palette; tracked here now so `palette::toggle` has somewhere
    /// to land and the `palette` key-context frame exists.
    palette_open: bool,
}

impl ShellView {
    pub fn new(services: ShellServices, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            services,
            matcher: Matcher::default(),
            focus_handle,
            palette_open: false,
        }
    }

    /// The active context stack for key resolution, outermost first:
    /// `workspace` is always active; `palette` layers on top while open.
    fn context_stack(&self) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
        if self.palette_open {
            stack.push(KeyContext::new("palette"));
        }
        stack
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(keystroke) = convert_keystroke(&event.keystroke) else {
            return;
        };
        let stack = self.context_stack();
        match self.matcher.press(&self.services.keymap, keystroke, &stack) {
            MatchResult::Matched(action) => {
                let handled = apply_workspace_action(&mut self.services.workspaces, &action);
                if !handled && action.0 == "palette::toggle" {
                    self.palette_open = !self.palette_open;
                }
                cx.notify();
            }
            MatchResult::Pending | MatchResult::NoMatch => {
                // The status bar shows pending keystrokes later (spec §3);
                // for now just repaint so nothing looks stuck.
                cx.notify();
            }
        }
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `viewport_size` is the drawable area (excludes window chrome),
        // which is what `Tree::layout` should partition (gpui/window.rs).
        // The status bar (Task 4) is a fixed-height strip below the tiles,
        // so the tile area gets the viewport minus that height; `Tree::
        // layout` is still called exactly once, over those shrunk bounds.
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width);
        let content_height = (f32::from(viewport.height) - status::HEIGHT).max(0.0);

        let (focused, rects) = {
            let tree = self.services.workspaces.active();
            (
                tree.focused(),
                tree.layout(Rect {
                    x: 0.0,
                    y: 0.0,
                    w: width,
                    h: content_height,
                }),
            )
        };

        // Fixed-height (not `size_full`) so it never competes with the
        // status bar for space below it: the tile tree is laid out over
        // exactly this height above, and the container must match.
        let mut surface = div().relative().w_full().h(px(content_height)).flex_none();
        if rects.is_empty() {
            surface = surface.flex().items_center().justify_center().child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("mod+s / mod+v to open a tile"),
            );
        } else {
            for (id, r) in rects {
                let is_focused = focused == Some(id);
                surface = surface.child(
                    div()
                        .absolute()
                        .left(px(r.x + 1.0))
                        .top(px(r.y + 1.0))
                        .w(px((r.w - 2.0).max(0.0)))
                        .h(px((r.h - 2.0).max(0.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(cx.theme().background)
                        .border_color(if is_focused {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                        .when(is_focused, |el| el.border_2())
                        .when(!is_focused, |el| el.border_1())
                        .text_color(cx.theme().muted_foreground)
                        // Click-to-focus is a convenience: keyboard (hjkl)
                        // remains the primary path through the same
                        // `Tree::focus` verb `apply_workspace_action` uses.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, _event, _window, cx| {
                                view.services.workspaces.active_mut().focus(id);
                                cx.notify();
                            }),
                        )
                        .child(format!("tile {}", id.0)),
                );
            }
        }

        let non_empty = self.services.workspaces.non_empty_indices();
        let status_bar = status::status_bar(
            self.services.workspaces.active_index(),
            &non_empty,
            self.matcher.pending(),
            "default", // Task 5 wires the real theme name.
            cx,
        );

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(surface)
            .child(status_bar)
            // ShellView is the first-level view Root wraps; Root's own
            // Render impl does not paint these overlay layers itself, so
            // whoever it wraps must (spec: gpui-component usage.md "Overlay
            // Layers"). Task 6's palette/dialogs need this in place now.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
    use crate::keymap::build_keymap;
    use geode_core::config::{ConfigSources, LayerDoc};

    fn test_services() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
        }
    }

    /// End-to-end: a real `mod+s` keystroke, dispatched through gpui's own
    /// key-event pipeline (not called directly), lands on `ShellView` and
    /// changes workspace state. Exercises `convert_keystroke` -> `Matcher`
    /// -> `apply_workspace_action` wired the way the render path wires them.
    #[gpui::test]
    fn mod_s_keystroke_splits_the_active_workspace(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        // Force a paint so the key-listener dispatch tree is registered
        // before we simulate a keystroke against it.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("alt-s");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "alt-s (mod+s = workspace::split_horizontal) should have created the first tile \
             on the empty starting workspace"
        );

        // The tile render path (Task 3) paints a background/border quad per
        // visible tile, not just text; a non-empty scene after the split is
        // cheap evidence the tiling surface actually drew something (the
        // geometry itself is tiling::tree's job, already unit-tested there).
        let quads_after_split = cx.update(|window, _cx| window.painted_quads().len());
        assert!(
            quads_after_split > 0,
            "expected the single tile to paint at least one quad"
        );
    }

    /// Layers a test-only `"g g"` sequence binding on top of the builtin
    /// keymap (spec §3.4: sequence bindings), so the status bar's
    /// pending-keystroke display (Task 4) has something real to show. No
    /// builtin binding starts a sequence today, so this is the cheapest
    /// honest way to exercise it without waiting on Task 6's palette-Esc
    /// flow.
    fn test_services_with_gg_binding() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        registry
            .register(crate::actions::ActionDef {
                id: crate::actions::ActionId("test::gg".to_string()),
                title: "Test gg".to_string(),
                category: "Test".to_string(),
            })
            .unwrap();
        let mod_alias = default_mod();
        let builtin_doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let user_doc = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".to_string(),
            file: "<test:user>".into(),
            table: "[[bindings]]\n[bindings.keys]\n\"g g\" = \"test::gg\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[builtin_doc, user_doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
        }
    }

    /// Pressing the first `g` of a `"g g"` sequence leaves the matcher
    /// pending (which the status bar renders as `"g"`) and the window still
    /// draws cleanly — the status bar's pending-keystroke path is live end
    /// to end through the real key-event pipeline.
    #[gpui::test]
    fn first_key_of_a_sequence_leaves_pending_keys_and_still_draws(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view =
                        cx.new(|cx| ShellView::new(test_services_with_gg_binding(), window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("g");

        // The pending keystroke must not stall the render thread (spec
        // PHILOSOPHY.md): the status bar draws the same frame it renders in.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let pending_len = shell.read_with(&cx, |shell, _| shell.matcher.pending().len());
        assert_eq!(
            pending_len, 1,
            "first 'g' of the 'g g' sequence should leave one pending keystroke"
        );
    }
}
