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

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{KeyContext, Keymap, MatchResult, Matcher, Modifiers};
use crate::palette::{self, PaletteItem, PaletteState};
use crate::theme::ThemeService;
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
    pub theme: ThemeService,
}

/// The window's root view. Intercepts all keyboard input via `on_key_down`
/// rather than gpui's own action-dispatch system, because key resolution
/// here goes through the shell's own layered, sequence-aware [`Matcher`]
/// (spec §3.4), not a static `KeyBinding` table.
pub struct ShellView {
    services: ShellServices,
    matcher: Matcher,
    focus_handle: FocusHandle,
    /// The open command palette's state (Task 6), or `None` when closed.
    /// Built fresh from the registry/keymap/theme service each time
    /// `palette::toggle` opens it (brief: the reverse binding index is
    /// built once at palette-open, not per frame) and dropped on close —
    /// nothing about it survives being closed and reopened.
    palette: Option<PaletteState>,
}

impl ShellView {
    pub fn new(services: ShellServices, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            services,
            matcher: Matcher::default(),
            focus_handle,
            palette: None,
        }
    }

    /// The active context stack for key resolution, outermost first:
    /// `workspace` is always active; `palette` layers on top while open.
    /// Currently only consulted by [`is_palette_toggle`](Self::is_palette_toggle)
    /// (to gate that binding's own `context`, if a user keymap ever adds
    /// one) — `handle_key_down` never reaches `self.matcher.press` while
    /// `self.palette` is `Some`, since palette-open key handling is
    /// exclusive (see that method's doc comment).
    fn context_stack(&self) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
        if self.palette.is_some() {
            stack.push(KeyContext::new("palette"));
        }
        stack
    }

    /// True if `keystroke` exactly matches a single-key binding for
    /// `palette::toggle`. Checked directly against the keymap rather than
    /// through `self.matcher`, so the toggle key can open *and* close the
    /// palette without ever touching (or being confused by) the matcher's
    /// own pending-sequence state, which palette-open key handling
    /// bypasses entirely.
    ///
    /// The keymap's layering contract is last-exact-match-wins (mirrors
    /// `Matcher::press`, spec §3.4): among every single-keystroke binding
    /// for this exact key whose predicate passes the current context
    /// stack, the *last* one in `Keymap::bindings()`'s layer-then-
    /// declaration order is the one that actually governs the key — a
    /// user/desk layer rebinding or unbinding (`"mod+p" = "none"`) it must
    /// shadow the builtin `palette::toggle` binding here exactly as it
    /// would through the matcher. So this resolves that same winning
    /// binding and only treats the keystroke as the palette toggle when
    /// its action is `palette::toggle`.
    fn is_palette_toggle(&self, keystroke: &crate::keymap::Keystroke) -> bool {
        let stack = self.context_stack();
        let winner = self.services.keymap.bindings().iter().rfind(|binding| {
            binding.keystrokes.len() == 1
                && binding.keystrokes[0] == *keystroke
                && binding.predicate.as_ref().is_none_or(|p| p.eval(&stack))
        });
        winner.is_some_and(|binding| binding.action.0 == "palette::toggle")
    }

    /// Open the palette (building a fresh `PaletteState` — actions in
    /// registry order, then themes) if it's closed, or close it if it's
    /// open.
    fn toggle_palette(&mut self) {
        if self.palette.is_some() {
            self.palette = None;
            return;
        }
        let bindings = palette::build_binding_index(&self.services.keymap);
        let items = palette::build_items(&self.services.registry, &self.services.theme, &bindings);
        self.palette = Some(PaletteState::new(items));
    }

    /// Apply one resolved action id through the shell's one dispatch chain
    /// (spec: "one keymap, ours" — no parallel action-dispatch system).
    /// Workspace verbs go through `apply_workspace_action`; the shell's own
    /// non-workspace actions (`palette::toggle`, `theme::toggle_mode`) are
    /// handled here when that leaves them unhandled. Shared by the normal
    /// keymap-matcher path and the palette's Enter-to-dispatch path, so
    /// both take exactly the same action to the same place.
    fn dispatch(&mut self, action: &ActionId, cx: &mut Context<Self>) {
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if !handled && action.0 == "palette::toggle" {
            self.toggle_palette();
        } else if !handled && action.0 == "theme::toggle_mode" {
            self.services.theme.toggle_mode(cx);
        }
    }

    /// Dispatch one selected palette row: an `Action` item goes through the
    /// normal [`dispatch`](Self::dispatch) chain (brief: "action -> the
    /// normal dispatch chain incl. theme::toggle_mode"); a `Theme` item
    /// applies that theme directly via `ThemeService::apply`. The palette
    /// is assumed already closed by the caller (Enter closes before
    /// dispatching) — so the `palette::toggle` action id is deliberately
    /// *not* re-dispatched here: `dispatch`'s `palette::toggle` branch
    /// calls `toggle_palette`, which would reopen the just-closed palette,
    /// turning "select 'Toggle command palette'" into "close then
    /// immediately reopen". Skipping it instead makes selecting that row a
    /// true toggle: the palette just closes and stays closed, exactly like
    /// pressing the toggle keystroke a second time would.
    fn dispatch_palette_item(&mut self, item: &PaletteItem, cx: &mut Context<Self>) {
        match item {
            PaletteItem::Action(id, ..) if id.0 == "palette::toggle" => {}
            PaletteItem::Action(id, ..) => self.dispatch(id, cx),
            PaletteItem::Theme(name) => {
                // The name is already fully qualified (e.g. "Gruvbox
                // Dark"), which `ThemeService::resolve` matches outright
                // regardless of the `mode` argument — so the mode passed
                // here is irrelevant to which theme gets applied.
                self.services
                    .theme
                    .apply(name, crate::theme::Mode::Dark, cx);
            }
        }
    }

    /// Handle one key event while the palette is open. Exclusive routing
    /// (plan constraint: "keyboard-first ... open, type, navigate,
    /// dispatch, close"; brief: "Palette-open swallows all other bindings
    /// ... keys go to the palette handler exclusively") — the shell's own
    /// keymap `Matcher` is never consulted here, so no other binding (a
    /// sequence, a workspace verb, anything) can leak through while typing
    /// a query. The palette-toggle keystroke itself is intercepted earlier
    /// in `handle_key_down`, before this method ever runs, so it does not
    /// need a case here.
    ///
    /// Reads gpui's own `Keystroke` directly (`event.keystroke`, not the
    /// shell-native one `convert_keystroke` produces) because free text
    /// entry needs `key_char` (the actual typed/shifted character) and
    /// named keys (`"backspace"`, `"up"`, `"down"`, `"enter"`, `"escape"`)
    /// that the shell-native conversion's matcher-oriented shape doesn't
    /// carry.
    fn handle_palette_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let mods = ks.modifiers;

        match ks.key.as_str() {
            "escape" => self.palette = None,
            "enter" => {
                let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
                self.palette = None;
                if let Some(item) = selected {
                    self.dispatch_palette_item(&item, cx);
                }
            }
            "backspace" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.backspace();
                }
            }
            "up" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
            }
            "down" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
            }
            "p" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
            }
            "n" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
            }
            _ => {
                // Plain typing only: a chord that also holds ctrl/cmd/fn
                // is a shortcut, not text entry, even if the platform
                // still reports a `key_char` for it.
                if !mods.control
                    && !mods.platform
                    && !mods.function
                    && let (Some(chars), Some(palette)) =
                        (ks.key_char.as_ref(), self.palette.as_mut())
                {
                    for c in chars.chars() {
                        palette.push_char(c);
                    }
                }
            }
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(keystroke) = convert_keystroke(&event.keystroke)
            && self.is_palette_toggle(&keystroke)
        {
            self.toggle_palette();
            cx.notify();
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(event, cx);
            cx.notify();
            return;
        }

        let Some(keystroke) = convert_keystroke(&event.keystroke) else {
            return;
        };
        let stack = self.context_stack();
        match self.matcher.press(&self.services.keymap, keystroke, &stack) {
            MatchResult::Matched(action) => {
                self.dispatch(&action, cx);
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
            self.services.theme.active_name(),
            cx,
        );

        let viewport_height = f32::from(viewport.height);

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(surface)
            .child(status_bar)
            // The palette overlay paints above the tiles/status bar (later
            // children paint above earlier siblings) but below gpui-
            // component's own dialog/notification layers below.
            .when_some(self.palette.as_ref(), |el, state| {
                el.child(palette::render(state, width, viewport_height, cx))
            })
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
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
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

    /// End-to-end: a real `mod+shift+t` keystroke, dispatched through gpui's
    /// own key-event pipeline, flips the active theme's mode. Exercises the
    /// same wiring as `mod_s_keystroke_splits_the_active_workspace` above,
    /// but through the `theme::toggle_mode` branch of `handle_key_down`
    /// added in Task 5.
    #[gpui::test]
    fn mod_shift_t_keystroke_toggles_the_theme_mode(cx: &mut gpui::TestAppContext) {
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

        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });

        cx.simulate_keystrokes("alt-shift-t");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let after = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_ne!(
            before, after,
            "alt-shift-t (mod+shift+t = theme::toggle_mode) should have changed the active theme"
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
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
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

    /// End-to-end command palette flow (Task 6), through the real
    /// key-event pipeline exactly like the tests above: `mod+p` opens it,
    /// typing "split" filters the list down to "Split horizontal" and
    /// "Split vertical" — the only two titles containing that whole run
    /// as a subsequence, and, having matched the identical literal
    /// prefix "split", scored *identically* by `fuzzy_match` (verified by
    /// hand: both score 45). Which one lands at index 0 is not a
    /// fuzzy-match property; it's `PaletteState::filtered`'s stable sort
    /// preserving `build_items`' input order, which is
    /// `ActionRegistry::iter()`'s `BTreeMap<ActionId, _>` order — and
    /// `"workspace::split_horizontal" < "workspace::split_vertical"`
    /// (`h` < `v`) puts horizontal first. That tie-break is deterministic
    /// (so this test is not flaky), just not the "the only match" story a
    /// prior version of this comment told. Enter then dispatches the
    /// selected item through the normal chain, closing the palette and
    /// splitting the (until then empty) active workspace.
    #[gpui::test]
    fn mod_p_opens_types_filters_and_enter_dispatches_the_selected_action(
        cx: &mut gpui::TestAppContext,
    ) {
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

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "palette starts closed"
        );

        cx.simulate_keystrokes("alt-p");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "alt-p (mod+p = palette::toggle) should have opened the palette"
        );

        cx.simulate_input("split");
        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Split horizontal".to_string()),
            "typing \"split\" should rank \"Split horizontal\" first, ahead of the \
             equally-scored \"Split vertical\", via the registry's alphabetical \
             (h < v) ActionId order and filtered()'s stable sort"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter should close the palette"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "enter on \"Split horizontal\" should have dispatched \
             workspace::split_horizontal through the normal chain"
        );
    }

    /// End-to-end: Enter on a *theme* row (not an action) changes the
    /// active theme, through the same real key-event pipeline as the
    /// action-dispatch test above — the brief-mandated "theme item ->
    /// `ThemeService::apply`" path had no direct test coverage before
    /// this one; it was previously verified only by reading
    /// `dispatch_palette_item`'s source.
    ///
    /// Query "gruvbox" ranks "Theme: Gruvbox Dark" and "Theme: Gruvbox
    /// Light" identically (both match the literal, fully-consecutive run
    /// "gruvbox" right after the "Theme: " word boundary — same
    /// computation as any other title sharing that whole run, so same
    /// score); no other registered action or bundled theme title contains
    /// "gruvbox" as a subsequence at all, bundled or not, so those two are
    /// the entire tied-for-first set. As in the split-horizontal test
    /// above, which one lands at index 0 is a deterministic tie-break —
    /// `build_items` appends themes in `ThemeService::names()`'s sorted
    /// order, and `"Gruvbox Dark" < "Gruvbox Light"` alphabetically — not
    /// a property of the fuzzy match itself.
    #[gpui::test]
    fn mod_p_opens_types_filters_and_enter_dispatches_the_selected_theme(
        cx: &mut gpui::TestAppContext,
    ) {
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

        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_ne!(
            before, "Gruvbox Dark",
            "the starting theme must differ from the target so the assertion \
             below actually proves something changed"
        );

        cx.simulate_keystrokes("alt-p");
        cx.simulate_input("gruvbox");

        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Theme: Gruvbox Dark".to_string()),
            "typing \"gruvbox\" should rank \"Theme: Gruvbox Dark\" first, ahead of \
             the equally-scored \"Theme: Gruvbox Light\", via ThemeService::names()'s \
             alphabetical order and filtered()'s stable sort"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter should close the palette"
        );
        let after = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_eq!(
            after, "Gruvbox Dark",
            "enter on \"Theme: Gruvbox Dark\" should have dispatched it through \
             ThemeService::apply, changing the active theme"
        );
    }

    /// Esc closes the palette without dispatching anything — typing a
    /// query that would otherwise match and select an action must not
    /// leave any trace once the palette is dismissed.
    #[gpui::test]
    fn escape_closes_the_palette_without_dispatching(cx: &mut gpui::TestAppContext) {
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

        cx.simulate_keystrokes("alt-p");
        cx.simulate_input("split");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("escape");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "escape should close the palette"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "escape must not dispatch the item that was filtered/selected"
        );
    }

    /// Layers a user binding on top of the builtin keymap that rebinds
    /// `mod+p` (BUILTIN_KEYMAP's `palette::toggle` key) to
    /// `workspace::split_horizontal` instead. Per the layering contract
    /// (last-exact-match-wins), this must fully shadow the builtin
    /// `palette::toggle` binding for that key.
    fn test_services_with_mod_p_rebound_to_split() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let builtin_doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let user_doc = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".to_string(),
            file: "<test:user>".into(),
            table: "[[bindings]]\n[bindings.keys]\n\"mod+p\" = \"workspace::split_horizontal\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[builtin_doc, user_doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
        }
    }

    /// Regression for `is_palette_toggle` respecting keymap layering
    /// (last-exact-match-wins, spec §3.4): a user layer rebinding `mod+p`
    /// away from `palette::toggle` must mean pressing it does NOT open the
    /// palette — the pre-matcher intercept in `handle_key_down` must not
    /// fire just because *some* binding for that key, anywhere in the
    /// keymap, happens to be `palette::toggle`. The rebound action
    /// (`workspace::split_horizontal`) must dispatch instead, through the
    /// normal matcher path, proving the key was fully handed over rather
    /// than merely swallowed.
    #[gpui::test]
    fn user_layer_rebinding_mod_p_prevents_palette_open_and_dispatches_rebound_action(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services_with_mod_p_rebound_to_split(), window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("alt-p");

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

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "a user layer rebinding mod+p away from palette::toggle must shadow the \
             builtin binding — the palette must not open"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "alt-p should have dispatched the rebound workspace::split_horizontal \
             action through the normal matcher path"
        );
    }

    /// Regression for `dispatch_palette_item`: selecting the
    /// `palette::toggle` row from inside the palette itself is a true
    /// toggle — the palette closes (Enter already did that) and must stay
    /// closed, not reopen. Filters straight down to that one row via its
    /// exact title so the test doesn't depend on where it ranks unfiltered.
    #[gpui::test]
    fn enter_on_the_palette_toggle_row_closes_the_palette_without_reopening(
        cx: &mut gpui::TestAppContext,
    ) {
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

        cx.simulate_keystrokes("alt-p");
        cx.simulate_input("Toggle command palette");

        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Toggle command palette".to_string()),
            "the query should have filtered down to exactly that row"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter on the palette::toggle row must leave the palette closed, not \
             reopen it"
        );
    }
}
