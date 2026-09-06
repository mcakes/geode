//! Shared test scaffolding for `ShellView`'s test suite (moved out of
//! `shell/mod.rs`, Task 0 part 2 — a pure move). Each seam below is one
//! test file, in the order the tests used to sit in the single file;
//! helpers used by more than one seam live here as `pub(super) fn`.

use super::*;
use crate::actions::ActionId;
use crate::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
use crate::keymap::build_keymap;
use crate::tiling::{DockSide, Rect};
use geode_core::config::{ConfigSources, Layer, LayerDoc};
use gpui::{MouseButton, MouseDownEvent, MouseUpEvent, div, px};
use gpui_component::{Root, TITLE_BAR_HEIGHT};
// `WindowExt` and (since Task 4) `Focusable` are already brought in by
// `use super::*` (top-of-file imports in `shell/mod.rs`) — needed by
// `handle_key_down`'s dialog guard and `filter_is_focused`/
// `dialog_filter_is_focused`'s `.focus_handle(cx)` calls below,
// respectively.

pub(super) fn test_services() -> ShellServices {
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
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
    }
}

/// `test_services` with a recording module as the default occupant.
pub(super) fn services_with_recorder() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let recorder = crate::module::recording::RecordingFactory::new("rec");
    let log = recorder.log.clone();
    let mut services = test_services();
    let mut roster = crate::module::ModuleRoster::new("rec");
    roster.add(Box::new(recorder));
    roster.register_actions(&mut services.registry);
    // The keymap must be rebuilt after the module's actions exist,
    // exactly as `main.rs` orders it, plus a binding into the
    // module's own context so a key can be seen to reach it.
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let module_doc = LayerDoc::builtin(
        "keymap",
        "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"j\" = \"rec::noop\"\n",
    )
    .unwrap();
    let (keymap, diags) = build_keymap(&[doc, module_doc], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services.roster = roster;
    (services, log)
}

pub(super) fn open_shell(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (window, vcx)
}

pub(super) fn shell_of(
    window: &gpui::WindowHandle<Root>,
    cx: &mut gpui::VisualTestContext,
) -> Entity<ShellView> {
    window.root(cx).unwrap().read_with(cx, |root, _| {
        root.view().clone().downcast::<ShellView>().unwrap()
    })
}

/// Shared scaffolding for the dock e2e tests below (dock-regions task):
/// open a window over a fresh `ShellView`, draw once so the key
/// dispatch tree exists, and hand back the visual context plus the
/// downcast shell entity — the exact setup every other e2e test here
/// builds inline.
pub(super) fn dock_test_shell(
    cx: &mut gpui::TestAppContext,
) -> (gpui::VisualTestContext, Entity<ShellView>) {
    cx.update(gpui_component::init);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
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
    (cx, shell)
}
/// Open a real window with a real `ShellView`, draw a frame, dispatch
/// `action`, draw again — the preamble every dialog test here needs.
pub(super) fn dialog_test_shell(
    cx: &mut gpui::TestAppContext,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    // Same reclaimed keybindings `main` registers in production
    // (`dialog::init_reclaimed_keybindings`'s own doc comment has the
    // full mechanism for each) — without this, a dialog test that
    // presses tab would prove nothing: gpui-component's `Root` would
    // still silently consume it exactly as it does in an unpatched
    // window.
    cx.update(dialog::init_reclaimed_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root = window.root(&mut vcx).unwrap();
    let shell = root.read_with(&vcx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (shell, vcx)
}
/// Does the shared dialog filter currently hold focus?
pub(super) fn dialog_filter_is_focused(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        shell
            .read(cx)
            .dialog_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    })
}
/// Does the scope bar's live text field (Task 4, spec §3.11) currently
/// hold focus?
pub(super) fn filter_is_focused(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        shell
            .read(cx)
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    })
}
/// Layers a test-only `"g g"` sequence binding on top of the builtin
/// keymap (spec §3.4: sequence bindings), so the status bar's
/// pending-keystroke display (Task 4) has something real to show. The
/// builtin keymap has no sequence bindings anymore (move-tile went
/// direct to `ctrl+alt+arrows`), so this isolated binding is the way
/// tests exercise a pending keystroke at all.
pub(super) fn test_services_with_gg_binding() -> ShellServices {
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
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
    }
}
/// `apply_reload` is `ShellView`'s real config-hot-reload apply path
/// (Task 1c-1); the watcher task is just what schedules calling it —
/// gpui's test executor never advances its simulated clock on
/// `run_until_parked` (confirmed against the pinned rev's
/// `TestScheduler::run`, which is a plain `while step() {}` with no
/// clock advancement), so there's no practical way to drive a ~500ms
/// polling loop through a `#[gpui::test]`. These tests call
/// `apply_reload` directly through the real entity instead — still a
/// real-entity test, exercising the exact method the watcher calls.
pub(super) fn config_with_mod(mod_key: &str) -> Config {
    Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("app", &format!("[keymap]\nmod = \"{mod_key}\"\n")).unwrap(),
        ],
        desk: None,
        user: None,
    })
}

mod chrome_and_dialogs;
mod commandline;
mod dock;
mod drag;
mod keybindings_dialog;
mod occupants;
mod palette;
mod perf;
mod reload;
mod scopebar;
mod session;
mod tiling_keys;
