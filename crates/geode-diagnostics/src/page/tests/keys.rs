//! Keys through the real keymap: the shell's builtin document and this
//! page's fragment, matched under the context stack the shell gives an
//! open page (`page`, then the page's own context), then dispatched to
//! the page as the shell's page door does.

use super::*;
use geode_core::config::LayerDoc;
use geode_core::log::Level;
use geode_shell::actions::ActionRegistry;
use geode_shell::keymap::{
    KeyContext, Keymap, MatchResult, Matcher, build_keymap, fragments::fragment_doc, parse_binding,
};
use geode_shell::module::PageFactory as _;

fn keymap() -> Keymap {
    let mut registry = ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    crate::DiagnosticsPageFactory::new(Arc::new(Ring::new(4)), Config::default())
        .register_actions(&mut registry);
    let builtin = LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP).unwrap();
    let fragment = fragment_doc("diagnostics", crate::DEFAULT_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(
        &[builtin, fragment],
        geode_shell::defaults::default_mod(),
        &registry,
    );
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

/// What `spec` (a key sequence, `"z shift+r"`) resolves to under the
/// page's current context, without dispatching it.
fn resolve(h: &Harness, vcx: &gpui::VisualTestContext, spec: &str) -> Option<ActionId> {
    let keymap = keymap();
    let stack = [
        KeyContext::new("page"),
        h.page.read_with(vcx, |p, _| p.key_context()),
    ];
    let mut matcher = Matcher::default();
    let mut last = None;
    for ks in parse_binding(spec, geode_shell::defaults::default_mod()).unwrap() {
        last = match matcher.press(&keymap, ks, &stack) {
            MatchResult::Matched { action, .. } => Some(action),
            MatchResult::Pending | MatchResult::NoMatch => None,
        };
    }
    last
}

/// Press `spec` and dispatch what it resolves to; panics when nothing
/// binds it. Returns whether the page consumed the action.
pub(super) fn key(h: &Harness, vcx: &mut gpui::VisualTestContext, spec: &str) -> bool {
    let action = resolve(h, vcx, spec).unwrap_or_else(|| panic!("{spec}: no binding"));
    let consumed = vcx.update(|window, cx| {
        h.page
            .update(cx, |p, cx| p.dispatch(&action, None, window, cx))
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    consumed
}

fn rows(h: &Harness, vcx: &gpui::VisualTestContext) -> usize {
    h.page.read_with(vcx, |p, _| p.prepared().rows.len())
}

fn open_dataset(h: &Harness, vcx: &mut gpui::VisualTestContext) {
    h.diagnostics.update(vcx, |d, cx| {
        d.set_catalog(
            snapshot_with(
                geode_core::query::AsOf::Live,
                vec![crate::model::tests::dataset_catalog()],
            ),
            SystemTime::now(),
        );
        cx.notify();
    });
    open_data_section(h, vcx);
    focus_page(h, vcx);
}

#[gpui::test]
fn space_toggles_a_dataset_row_as_enter_does(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_dataset(&h, &mut vcx);
    assert_eq!(rows(&h, &vcx), 3, "parent + 2 generations");
    assert!(key(&h, &mut vcx, "space"));
    assert_eq!(rows(&h, &vcx), 1, "space collapses the parent");
    assert!(key(&h, &mut vcx, "space"));
    assert_eq!(rows(&h, &vcx), 3, "and expands it again");
    assert!(key(&h, &mut vcx, "enter"));
    assert_eq!(rows(&h, &vcx), 1, "enter is the same toggle");
}

/// With the filter focused the page's context is `mode == insert`, where
/// nothing binds a bare space, so the shell's insert route leaves it to
/// the input.
#[gpui::test]
fn space_in_the_filter_types_a_space(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_dataset(&h, &mut vcx);
    key(&h, &mut vcx, "/");
    vcx.run_until_parked();
    assert!(h.page.read_with(&vcx, |p, _| p.insert_mode));
    assert_eq!(resolve(&h, &vcx, "space"), None, "nothing claims the space");
    vcx.simulate_input("a");
    vcx.simulate_keystrokes("space");
    vcx.simulate_input("b");
    vcx.run_until_parked();
    assert_eq!(
        h.page
            .read_with(&vcx, |p, _| p.filters[Section::Data as usize].clone()),
        "a b"
    );
}

#[gpui::test]
fn tab_and_shift_tab_step_the_config_views(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    note_two_config_batches(&h, &mut vcx);
    open_config_section(&h, &mut vcx);
    focus_page(&h, &mut vcx);
    let view = |vcx: &gpui::VisualTestContext| {
        h.page
            .read_with(vcx, |p, _| (p.config_history, p.config_values))
    };
    assert_eq!(view(&vcx), (false, false), "Current issues");
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(view(&vcx), (true, false), "History");
    assert!(key(&h, &mut vcx, "tab"));
    assert!(view(&vcx).1, "Effective values");
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(view(&vcx), (false, false), "wraps to Current issues");
    assert!(key(&h, &mut vcx, "shift+tab"));
    assert!(view(&vcx).1, "back to Effective values");
    assert!(key(&h, &mut vcx, "shift+tab"));
    assert_eq!(view(&vcx), (true, false), "back to History");
    assert!(key(&h, &mut vcx, "ctrl+tab"), "ctrl+tab stays");
    assert!(view(&vcx).1);
    // A section without views consumes tab and changes nothing.
    key(&h, &mut vcx, "g s");
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
}

#[gpui::test]
fn o_opens_the_config_directory_only_from_config(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    focus_page(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "o"));
    assert!(h.actions.borrow().is_empty(), "Sources has no such button");
    open_config_section(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "o"));
    assert_eq!(
        *h.actions.borrow(),
        vec!["config::open_directory".to_string()]
    );
}

#[gpui::test]
fn dataset_fold_all_and_reset_filters_take_their_shifted_and_alt_keys(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    open_dataset(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "z shift+m"));
    assert_eq!(rows(&h, &vcx), 1, "collapse all");
    assert!(key(&h, &mut vcx, "z shift+r"));
    assert_eq!(rows(&h, &vcx), 3, "expand all");
    key(&h, &mut vcx, "/");
    vcx.simulate_input("nomatch");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(!h.page.read_with(&vcx, |p, _| p.insert_mode));
    assert_eq!(rows(&h, &vcx), 0);
    assert!(key(&h, &mut vcx, "alt+backspace"));
    assert_eq!(rows(&h, &vcx), 3, "alt+backspace resets the filter");
}

#[gpui::test]
fn minus_and_equals_step_the_minimum_log_level(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_log_section(&h, &mut vcx);
    focus_page(&h, &mut vcx);
    let levels = |vcx: &gpui::VisualTestContext| h.page.read_with(vcx, |p, _| p.log_filter.levels);
    assert_eq!(levels(&vcx), [true; 5]);
    assert!(key(&h, &mut vcx, "-"));
    assert_eq!(levels(&vcx), [true, true, true, true, false], "TRACE off");
    for _ in 0..5 {
        key(&h, &mut vcx, "-");
    }
    assert_eq!(
        levels(&vcx),
        [true, false, false, false, false],
        "ERROR stays"
    );
    assert!(key(&h, &mut vcx, "="));
    assert_eq!(levels(&vcx), [true, true, false, false, false], "WARN back");
    // A hand-picked set steps from its most verbose level shown.
    click(&mut vcx, "diagnostics-level-WARN");
    click(&mut vcx, "diagnostics-level-INFO");
    assert_eq!(levels(&vcx), [true, false, true, false, false]);
    key(&h, &mut vcx, "=");
    assert_eq!(levels(&vcx), [true, true, true, true, false]);
}

#[gpui::test]
fn t_and_shift_t_step_the_log_target(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_log_section(&h, &mut vcx);
    push(&h.ring, Level::INFO, "geode::query", "planned");
    push(&h.ring, Level::INFO, "geode::shell", "drawn");
    notify(&h, &mut vcx);
    focus_page(&h, &mut vcx);
    let target =
        |vcx: &gpui::VisualTestContext| h.page.read_with(vcx, |p, _| p.log_filter.target.clone());
    assert!(key(&h, &mut vcx, "t"));
    assert_eq!(target(&vcx).as_deref(), Some("geode::query"));
    assert_eq!(rows(&h, &vcx), 1);
    key(&h, &mut vcx, "t");
    assert_eq!(target(&vcx).as_deref(), Some("geode::shell"));
    key(&h, &mut vcx, "t");
    assert_eq!(target(&vcx), None, "wraps to all targets");
    assert!(key(&h, &mut vcx, "shift+t"));
    assert_eq!(target(&vcx).as_deref(), Some("geode::shell"));
    vcx.run_until_parked();
    assert_eq!(
        h.page
            .read_with(&vcx, |p, cx| p
                .target_select
                .read(cx)
                .selected_value()
                .cloned())
            .as_deref(),
        Some("geode::shell"),
        "the select shows the stepped target"
    );
}

#[gpui::test]
fn ctrl_l_clears_the_log(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_log_section(&h, &mut vcx);
    push(&h.ring, Level::INFO, "geode::shell", "drawn");
    notify(&h, &mut vcx);
    focus_page(&h, &mut vcx);
    assert_eq!(rows(&h, &vcx), 1);
    assert!(key(&h, &mut vcx, "ctrl+l"));
    assert_eq!(rows(&h, &vcx), 0);
}

/// The Levels popover's buttons take no keyboard focus; `shift+l` opens
/// the shell's log-level chooser instead, closing the popover first.
/// (Escape over the popover is delivered as a real key through the shell
/// in `geode-app`'s `escape_over_the_levels_popover_closes_it_and_keeps_the_page`.)
#[gpui::test]
fn shift_l_opens_the_level_chooser(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    open_log_section(&h, &mut vcx);
    focus_page(&h, &mut vcx);
    click(&mut vcx, "diagnostics-levels-open");
    assert!(h.page.read_with(&vcx, |p, _| p.levels.open));
    assert!(key(&h, &mut vcx, "shift+l"));
    assert!(!h.page.read_with(&vcx, |p, _| p.levels.open));
    assert_eq!(*h.actions.borrow(), vec!["log::level".to_string()]);
}
