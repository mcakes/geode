//! Dispatch's action tail recording (Phase 4b Task 6): every dispatched
//! action's FNV-1a hash lands in `ShellServices::action_tail` before
//! `dispatch` matches the action — the crash hook's only view into
//! "what was the user doing" (`geode_app::crash::install_panic_hook`,
//! `ActionTail`'s own doc comment on why hashes rather than `ActionId`s).

use super::*;
use crate::actions::ActionId;
use crate::diagnostics::fnv1a;

#[gpui::test]
fn dispatching_three_actions_leaves_their_hashes_in_the_tail_in_order(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);

    // Three harmless, side-effect-free actions (no window-state
    // assumptions: a single-tile workspace still accepts a focus-left/
    // right dispatch as a no-op, per `apply_workspace_action`'s own doc
    // comment) — the point here is the tail, not what each one does.
    let dispatched = [
        "workspace::focus_left",
        "workspace::focus_right",
        "perf::toggle_overlay",
    ];

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            for id in dispatched {
                s.dispatch(&ActionId(id.to_string()), None, window, cx);
            }
        });
    });

    let recorded: Vec<u64> = shell.read_with(&vcx, |s, _| {
        s.services.action_tail.lock().unwrap().recent().collect()
    });
    let expected: Vec<u64> = dispatched.iter().map(|id| fnv1a(id)).collect();
    assert_eq!(
        recorded, expected,
        "the tail must hold the dispatched actions' hashes, oldest first"
    );
}
