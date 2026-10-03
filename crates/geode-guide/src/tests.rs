use super::*;
use geode_core::{groupings::GroupingSlots, log::LogLevels, scopes::SavedScopes};
use geode_shell::frame::Frame;
use geode_shell::keymap::{Keymap, MatchResult, Matcher};
use geode_shell::tiling::WorkspaceIx;
use gpui::prelude::*;
use gpui::{Context, FocusHandle, Render, TestAppContext, VisualTestContext, div};
use std::{cell::RefCell, rc::Rc};

#[test]
fn factory_fragment_is_valid_and_every_action_is_bound() {
    let mut roster = geode_shell::module::ModuleRoster::new();
    roster.add(Box::new(GuideFactory));
    let mut registry = ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    roster.register_actions(&mut registry);
    let (fragments, diagnostics) = roster.keymap_fragments();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let (keymap, diagnostics) = geode_shell::keymap::build_keymap(
        &fragments,
        geode_shell::defaults::default_mod(),
        &registry,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    for &(id, _) in ACTIONS {
        assert!(keymap.bindings().iter().any(|b| b.action.0 == id), "{id}");
    }
}

// The shell's normal-mode key route around the actual factory's occupant.
// It retains the ancestor focus target so button and text clicks exercise
// the same bubbling focus behavior as a hosted tile.
struct Host {
    view: gpui::AnyView,
    content: Rc<dyn TileContent>,
    focus: FocusHandle,
    keymap: Keymap,
    matcher: Matcher,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let Some(key) = geode_shell::shell::keys::convert_keystroke(&event.keystroke)
                else {
                    return;
                };
                let stack = [
                    KeyContext::new("workspace"),
                    KeyContext::new("tile"),
                    this.content.key_context(cx),
                ];
                if let MatchResult::Matched { action, count } =
                    this.matcher.press(&this.keymap, key, &stack)
                {
                    this.content.dispatch(&action, count, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.view.clone())
    }
}

struct Reader {
    content: Rc<dyn TileContent>,
    tile: Entity<GuideTile>,
    frame: Entity<Frame>,
}

fn open(cx: &mut TestAppContext, restored: Option<toml::Table>) -> (Reader, VisualTestContext) {
    cx.update(gpui_component::init);
    let mut registry = ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    let mut roster = geode_shell::module::ModuleRoster::new();
    roster.add(Box::new(GuideFactory));
    roster.register_actions(&mut registry);
    let (fragments, diagnostics) = roster.keymap_fragments();
    assert!(diagnostics.is_empty());
    let builtin =
        geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
            .unwrap();
    let docs = geode_shell::keymap::fragments::splice(&[builtin], &fragments);
    let (keymap, diagnostics) =
        geode_shell::keymap::build_keymap(&docs, geode_shell::defaults::default_mod(), &registry);
    assert!(diagnostics.is_empty());
    let slot = Rc::new(RefCell::new(None));
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                cx.set_global(geode_shell::tips::Chords(std::sync::Arc::new(
                    keymap.bindings().to_vec(),
                )));
                let frame =
                    cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                let occupant = GuideFactory.create(
                    TileId(1),
                    restored.as_ref(),
                    FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                    diagnostics,
                    window,
                    cx,
                );
                let tile = occupant.view.clone().downcast::<GuideTile>().unwrap();
                let content: Rc<dyn TileContent> = occupant.content.into();
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                content.set_visible(true, cx);
                *slot.borrow_mut() = Some(Reader {
                    content: content.clone(),
                    tile,
                    frame,
                });
                let host = cx.new(|_| Host {
                    view: occupant.view,
                    content,
                    focus,
                    keymap,
                    matcher: Matcher::default(),
                });
                cx.new(|cx| gpui_component::Root::new(host, window, cx))
            })
        })
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let reader = slot.borrow_mut().take().unwrap();
    (reader, vcx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = cx.debug_bounds(selector).expect("the control is painted");
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
}

impl Reader {
    fn section(&self, cx: &VisualTestContext) -> String {
        self.tile.read_with(cx, |t, _| {
            document::sections()[t.section].anchor.to_string()
        })
    }
    fn find(&self, event: FindEvent, cx: &mut VisualTestContext) {
        cx.update(|window, cx| self.content.find(event, window, cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn keyboard_and_pointer_navigate_and_restore_sections(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, None);
    let intro = reader.section(&vcx);
    vcx.simulate_keystrokes("]");
    assert_eq!(reader.section(&vcx), "how-to-think-about-geode");
    click(&mut vcx, "guide-next");
    assert_eq!(reader.section(&vcx), "a-first-walkthrough");
    vcx.simulate_keystrokes("c j enter");
    assert_eq!(reader.section(&vcx), "try-the-other-tools");
    vcx.simulate_keystrokes("c k escape");
    assert_eq!(
        reader.section(&vcx),
        "try-the-other-tools",
        "Escape only dismisses contents"
    );
    click(&mut vcx, "guide-contents");
    assert_eq!(
        vcx.update(|_, cx| reader
            .content
            .key_context(cx)
            .get("mode")
            .map(str::to_owned)),
        Some("menu".into())
    );
    click(&mut vcx, "guide-contents");
    assert_eq!(
        vcx.update(|_, cx| reader
            .content
            .key_context(cx)
            .get("mode")
            .map(str::to_owned)),
        Some("normal".into())
    );
    let saved = vcx.update(|_, cx| reader.content.serialize(cx));
    let (restored, _) = open(cx, Some(saved));
    assert_eq!(
        restored.tile.read_with(cx, |t, _| t.section),
        document::section_for("try-the-other-tools").unwrap()
    );
    vcx.simulate_keystrokes("9 9 [");
    assert_eq!(reader.section(&vcx), intro);
    click(&mut vcx, "guide-previous");
    assert_eq!(reader.section(&vcx), intro);
    vcx.simulate_keystrokes("c");
    vcx.update(|window, cx| {
        assert!(
            reader
                .content
                .dispatch(&ActionId("guide::next".into()), None, window, cx)
        );
    });
    assert_eq!(reader.section(&vcx), "how-to-think-about-geode");
    assert_eq!(
        vcx.update(|_, cx| reader
            .content
            .key_context(cx)
            .get("mode")
            .map(str::to_owned)),
        Some("normal".into())
    );
}

#[gpui::test]
fn scroll_keys_move_rendered_markdown_and_top_resets_it(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, Some(toml::toml! { section = "a-small-key-reference" }));
    // A small viewport forces the reference table to overflow.
    vcx.simulate_resize(gpui::size(gpui::px(560.), gpui::px(300.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let offset = |cx: &VisualTestContext| {
        reader.tile.read_with(cx, |t, cx| {
            t.text.read(cx).list_state().logical_scroll_top()
        })
    };
    let start = offset(&vcx);
    let viewport = reader
        .tile
        .read_with(&vcx, |t, cx| t.text.read(cx).list_state().viewport_bounds());
    assert!(
        viewport.size.height > gpui::px(0.) && viewport.size.width > gpui::px(0.),
        "the rendered Markdown must own a laid-out scroll viewport: {viewport:?}"
    );
    vcx.simulate_keystrokes("j");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let down = offset(&vcx);
    assert!(down.item_ix > start.item_ix || down.offset_in_item > start.offset_in_item);
    vcx.simulate_keystrokes("g g");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(offset(&vcx).item_ix, 0);
    assert_eq!(offset(&vcx).offset_in_item, gpui::px(0.));
}

#[gpui::test]
fn find_previews_sections_cancel_restores_and_committed_matches_cycle(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, Some(toml::toml! { section = "a-small-key-reference" }));
    reader.find(FindEvent::Changed("volatility".into()), &mut vcx);
    assert_ne!(reader.section(&vcx), "a-small-key-reference");
    reader.find(FindEvent::Cancelled, &mut vcx);
    assert_eq!(reader.section(&vcx), "a-small-key-reference");
    reader.find(FindEvent::Committed("scope".into()), &mut vcx);
    let first = reader.section(&vcx);
    vcx.simulate_keystrokes("n");
    assert_ne!(reader.section(&vcx), first);
    vcx.simulate_keystrokes("shift-n");
    assert_eq!(reader.section(&vcx), first);
    reader.find(FindEvent::Committed("no-such-guide-text".into()), &mut vcx);
    assert_eq!(reader.section(&vcx), first);
}

#[gpui::test]
fn unknown_restored_sections_fall_back_and_commands_stay_local(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, Some(toml::toml! { section = "removed-section" }));
    assert_eq!(reader.tile.read_with(&vcx, |t, _| t.section), 0);
    let before = reader.frame.read_with(&vcx, |f, _| f.shared().versions());
    vcx.update(|window, cx| {
        assert_eq!(
            reader.content.completions("section how", 3, cx),
            vec!["section"]
        );
        assert!(
            reader
                .content
                .completions("section how", 11, cx)
                .contains(&"how-to-think-about-geode".to_owned())
        );
        assert!(
            reader
                .content
                .command("section keep-a-workspaces-own-context", window, cx)
                .is_ok()
        );
        assert!(
            reader
                .content
                .command("section missing", window, cx)
                .is_err()
        );
    });
    assert_eq!(reader.section(&vcx), "keep-a-workspaces-own-context");
    assert_eq!(
        reader.frame.read_with(&vcx, |f, _| f.shared().versions()),
        before
    );
}

#[gpui::test]
fn a_narrow_reader_keeps_navigation_inside_the_tile_at_large_text(cx: &mut TestAppContext) {
    let (_, mut vcx) = open(cx, None);
    vcx.simulate_resize(gpui::size(gpui::px(240.), gpui::px(400.)));
    vcx.update(|window, cx| {
        gpui_component::Theme::global_mut(cx).font_size = gpui::px(20.);
        gpui_component::Theme::sync_base(cx);
        window.refresh();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let toolbar = vcx.debug_bounds("guide-toolbar").unwrap();
    for name in ["guide-contents", "guide-previous", "guide-next"] {
        let control = vcx.debug_bounds(name).unwrap();
        assert!(
            control.left() >= toolbar.left() && control.right() <= toolbar.right(),
            "{name}: {control:?}, {toolbar:?}"
        );
        assert!(
            control.top() >= toolbar.top() && control.bottom() <= toolbar.bottom(),
            "{name}: {control:?}, {toolbar:?}"
        );
    }
}

#[gpui::test]
fn a_frame_change_never_waits_for_the_offline_reader(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, None);
    reader.frame.update(&mut vcx, |frame, cx| {
        frame
            .shared_mut()
            .open_flip([geode_core::query::QueryKey(1)], std::time::Instant::now());
        assert!(frame.barrier_open());
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !reader
            .frame
            .read_with(&vcx, |frame, _| frame.barrier_open())
    );
}

#[gpui::test]
fn actions_menu_and_visible_hints_follow_the_live_keymap(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, None);
    assert!(vcx.debug_bounds("kbd:c").is_some());
    assert!(vcx.debug_bounds("kbd:/").is_some());
    assert!(vcx.debug_bounds("kbd:.").is_some());
    vcx.simulate_keystrokes(".");
    vcx.run_until_parked();
    click(&mut vcx, "guide-action-1-2");
    assert_eq!(reader.section(&vcx), "how-to-think-about-geode");
    click(&mut vcx, "guide-menu");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(vcx.debug_bounds("guide-actions-1").is_some());

    vcx.update(|window, cx| {
        use geode_shell::keymap::{Binding, Modifiers, parse_binding};
        let mut bindings = (*cx.global::<geode_shell::tips::Chords>().0).clone();
        for (keys, action) in [("c", "none"), ("shift+c", "guide::contents")] {
            bindings.push(Binding {
                keystrokes: parse_binding(keys, Modifiers::NONE).unwrap(),
                predicate: None,
                action: ActionId(action.into()),
                layer: geode_core::config::Layer::User,
                index: bindings.len(),
                context_source: None,
                key_source: keys.into(),
            });
        }
        cx.set_global(geode_shell::tips::Chords(std::sync::Arc::new(bindings)));
        window.refresh();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vcx.debug_bounds("kbd:shift-c").is_some(),
        "rebound key shown in controls and open menu"
    );
    assert!(
        vcx.debug_bounds("kbd:c").is_none(),
        "unbound default must disappear"
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(reader.section(&vcx), "how-to-think-about-geode");
}

fn painted_matches(reader: &Reader, vcx: &mut VisualTestContext) -> usize {
    use gpui_component::ActiveTheme as _;
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
        let bounds = reader
            .tile
            .read(cx)
            .text
            .read(cx)
            .bounds()
            .scale(window.scale_factor());
        let color = u32::from(gpui::Rgba::from(cx.theme().selection));
        window
            .painted_quads()
            .iter()
            .filter(|quad| {
                bounds.intersects(&quad.bounds)
                    && quad.background.as_solid().is_some_and(|fill| {
                        // HTML colors round-trip through 8-bit CSS channels.
                        u32::from(gpui::Rgba::from(fill))
                            .to_be_bytes()
                            .into_iter()
                            .zip(color.to_be_bytes())
                            .all(|(a, b)| a.abs_diff(b) <= 2)
                    })
            })
            .count()
    })
}

#[gpui::test]
fn search_paints_matches_in_prose_code_and_tables_and_escape_removes_them(cx: &mut TestAppContext) {
    let (reader, mut vcx) = open(cx, None);
    vcx.simulate_resize(gpui::size(gpui::px(560.), gpui::px(340.)));
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        vcx.update(|window, cx| gpui_component::Theme::change(mode, Some(window), cx));
        vcx.run_until_parked();
        for query in ["scope", "ctrl+k", ":range 1m", "Toggle fullscreen"] {
            reader.find(FindEvent::Committed(query.into()), &mut vcx);
            let painted = painted_matches(&reader, &mut vcx);
            assert!(painted > 0, "visible highlights for {query:?} in {mode:?}");
            assert!(
                reader
                    .tile
                    .read_with(&vcx, |t, _| t.search_status.contains("match"))
            );
            vcx.simulate_keystrokes("escape");
            vcx.run_until_parked();
            assert_eq!(
                painted_matches(&reader, &mut vcx),
                0,
                "Escape removes the marks"
            );
        }
    }
    reader.find(FindEvent::Committed("scope".into()), &mut vcx);
    vcx.simulate_keystrokes("n");
    vcx.run_until_parked();
    assert!(
        painted_matches(&reader, &mut vcx) > 0,
        "next section keeps its matches marked"
    );
    reader.find(FindEvent::Changed("no-such-text".into()), &mut vcx);
    assert!(
        reader
            .tile
            .read_with(&vcx, |t, _| t.search_status.starts_with("No matches"))
    );
    assert_eq!(painted_matches(&reader, &mut vcx), 0);
    reader.find(FindEvent::Cancelled, &mut vcx);
    assert_eq!(painted_matches(&reader, &mut vcx), 0);
}
