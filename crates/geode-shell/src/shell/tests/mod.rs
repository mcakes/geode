//! Shared test scaffolding for `ShellView`'s test suite (moved out of
//! `shell/mod.rs`, Task 0 part 2 — a pure move). Each seam below is one
//! test file, in the order the tests used to sit in the single file;
//! helpers used by more than one seam live here as `pub(super) fn`.

use super::*;
use crate::actions::ActionId;
use crate::defaults::{
    BUILTIN_KEYMAP, default_mod, register_builtin_actions, register_pick_actions,
    register_scope_actions,
};
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

/// The test layer every shell fixture stacks on `BUILTIN_KEYMAP`: the
/// shipped keymap has no create-a-tile chord any more (spec 2026-09-08
/// add-tile §3.1 — tiles are added by kind from the palette), so the
/// ~80 tests that say `ctrl-v`/`ctrl-h` keep meaning "add a recorder
/// tile, side by side / below" through these two bindings. They are
/// exactly the shape a desk keymap would ship (`tile::add_<kind>_*`),
/// not a private test-only action.
pub(super) const TEST_ADD_KEYMAP: &str = "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+v\" = \"tile::add_rec_horizontal\"\n\"ctrl+h\" = \"tile::add_rec_vertical\"\n";

/// `BUILTIN_KEYMAP` + [`TEST_ADD_KEYMAP`] + `extra`, built clean.
pub(super) fn test_keymap(registry: &ActionRegistry, extra: &[LayerDoc]) -> crate::keymap::Keymap {
    let mut docs = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
        LayerDoc::builtin("keymap", TEST_ADD_KEYMAP).unwrap(),
    ];
    docs.extend(extra.iter().cloned());
    let (keymap, diags) = build_keymap(&docs, default_mod(), registry);
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

pub(super) fn test_services() -> ShellServices {
    test_services_with_log().0
}

/// [`test_services`] plus the recorder's log — for a test that wants to
/// see what the "rec" occupant [`TEST_ADD_KEYMAP`]'s keys add was told.
pub(super) fn test_services_with_log() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    services_with_default_kind("placeholder")
}

/// The shell fixtures' one roster: a `RecordingFactory` of kind "rec" —
/// the kind [`TEST_ADD_KEYMAP`]'s `ctrl+v`/`ctrl+h` add, so a shell
/// built here really can add a tile twice and get two tiles (an add
/// onto a *placeholder* fills it in place, spec 2026-09-08 add-tile
/// §4.2, so a roster with no "rec" would collapse every second add into
/// the first tile). `default_kind` decides only what a tile created by
/// some *other* path gets — a direct `Workspaces::split_active`, or a
/// session record naming a kind nothing registered: "placeholder" for
/// [`test_services`], "rec" for [`services_with_recorder`].
fn services_with_default_kind(
    default_kind: &str,
) -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let config = Config::load(&ConfigSources::default());
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    // The startup ordering `main.rs` uses (`register_pick_actions` right
    // after `register_builtin_actions`, before `build_keymap`), exercised
    // here even though this config has no `datasets` doc — `pickable_
    // columns` then returns empty and the loop is a no-op, but the path
    // itself still runs on every test built from this fixture.
    register_pick_actions(&mut registry, &crate::shell::pickable_columns(&config));
    // Same reasoning as `register_pick_actions` just above, for the
    // `scope::<name>` actions (Phase 4a §3.11) — exercised here even
    // though this config has no `[scopes]` doc, so `crate::shell::
    // saved_scopes` returns empty and the loop is a no-op, but the
    // startup-ordering path itself still runs on every test built from
    // this fixture.
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&config, false));
    // The add rows for the recorder kind the shell tests use (spec
    // 2026-09-08 add-tile §3.2) — `main.rs` registers these from the
    // roster's kinds in this same slot, before `build_keymap`.
    crate::defaults::register_add_actions(&mut registry, &["rec"]);
    let recorder = crate::module::recording::RecordingFactory::new("rec");
    let log = recorder.log.clone();
    let mut roster = crate::module::ModuleRoster::new(default_kind);
    roster.add(Box::new(recorder));
    // Module actions exist before `build_keymap`, exactly as `main.rs`
    // orders it — a binding into the module's own context is what
    // `services_with_recorder`'s extra layer needs to resolve.
    roster.register_actions(&mut registry);
    let mod_alias = default_mod();
    let keymap = test_keymap(&registry, &[]);
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    let services = ShellServices {
        config,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster,
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
    };
    (services, log)
}

/// `test_services` with the recording module as the *default* occupant:
/// every tile, however it was created, is a recorder.
pub(super) fn services_with_recorder() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let (mut services, log) = services_with_default_kind("rec");
    // A binding into the module's own key context, so a key can be seen
    // to reach it.
    let module_doc = LayerDoc::builtin(
        "keymap",
        "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"j\" = \"rec::noop\"\n",
    )
    .unwrap();
    services.keymap = test_keymap(&services.registry, &[module_doc]);
    (services, log)
}

pub(super) fn open_shell(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    open_shell_inner(cx, services, None)
}

/// [`open_shell`] with a real, writable user config directory — the one
/// thing a test of a *persisting* action needs, since every persist path
/// in this crate (`persist_theme`, `keybindings_view::spawn_unbind`, …)
/// silently no-ops when `ShellView::user_dir` is `None`. Pass a
/// `tempfile::TempDir`'s path and keep the `TempDir` alive for the whole
/// test: the file is read back from it after `run_until_parked` drives
/// the background write to completion.
pub(super) fn open_shell_with_user_dir(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: &std::path::Path,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    open_shell_inner(cx, services, Some(user_dir.to_path_buf()))
}

fn open_shell_inner(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: Option<PathBuf>,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, user_dir, window, cx));
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
    let mut services = test_services();
    services
        .registry
        .register(crate::actions::ActionDef {
            id: crate::actions::ActionId("test::gg".to_string()),
            title: "Test gg".to_string(),
            category: "Test".to_string(),
        })
        .unwrap();
    let user_doc = LayerDoc {
        layer: geode_core::config::Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"g g\" = \"test::gg\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[user_doc]);
    services
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

mod asof;
mod chrome_and_dialogs;
mod commandline;
mod diagnostics;
mod dock;
mod drag;
mod flip;
mod input;
mod keybindings_dialog;
mod occupants;
mod palette;
mod perf;
mod picker;
mod reload;
mod scopebar;
mod session;
mod tiling_keys;
