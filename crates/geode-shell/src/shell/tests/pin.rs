//! Workspace-pinned frame: which lane a shell surface reads and commits to.

use super::occupants::dispatch_and_draw;
use super::*;
use crate::tiling::WorkspaceIx;

/// A frame dialog commits into the workspace it was opened from, even if
/// the active workspace changed underneath it.
#[gpui::test]
fn a_frame_dialog_commits_into_the_workspace_it_opened_from(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let ws1 = WorkspaceIx::FIRST;
    let ws2 = WorkspaceIx::new(2).unwrap();
    frame.update(&mut vcx, |f, _| assert!(f.pin(ws1)));
    dispatch_and_draw(&shell, &mut vcx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    // Test-only: move the active workspace underneath the open modal.
    shell.update(&mut vcx, |s, _| assert!(s.services.workspaces.switch(2)));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let (pinned, shared) = frame.read_with(&vcx, |f, _| {
        (
            f.view(ws1).scope().expression.is_some(),
            f.view(ws2).scope().expression.is_some(),
        )
    });
    assert!(pinned, "the expression lands in workspace 1's pinned lane");
    assert!(!shared, "the shared lane is untouched");
}
