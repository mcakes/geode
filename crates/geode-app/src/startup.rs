//! The windows the app opens at startup: one per restored `[windows.K]`
//! record, in K order, each on the index its record names and placed where
//! it was; with no usable record, one window on the session's initial
//! index where the platform puts it. The window count and placement live
//! here; what a window shows and how it opens is `ShellApp::open_window`.

use geode_shell::session::WindowBoundsRecord;
use geode_shell::shell::{Placement, ShellApp, WindowSpec};
use geode_shell::tiling::{WindowKey, WorkspaceIx};
use gpui::{App, Bounds, Entity, Pixels, point, px, size};

/// Open the startup windows through the installed opener and return their
/// keys in opening order. The restored records are taken from the app
/// once; the first record's window is the oldest, so it writes `active`
/// at the next save and hosts the restored indexes no window shows. A
/// record whose open is refused (its index is shown by an earlier record's
/// window, which the session reader already prevents; the opener failed)
/// is skipped with a warning and the rest still open. If none opened, one
/// window opens on the initial index at the platform's default placement;
/// a refusal there ends the run, since without a window there is nothing
/// to run. Lease-free: `open_window` leases the app only around its
/// reserve and confirm steps, never around the opener.
pub fn open_startup_windows(app: &Entity<ShellApp>, cx: &mut App) -> Vec<WindowKey> {
    let records = app.update(cx, |app, _| app.take_restored_windows());
    let mut opened = Vec::with_capacity(records.len().max(1));
    for record in records {
        let workspace = WorkspaceIx::new(record.workspace)
            .expect("the session reader keeps only records with an index in 1–9");
        let spec = WindowSpec {
            workspace,
            placement: Placement::Restored {
                display: record.display,
                bounds: record.bounds.map(bounds_of),
                maximized: record.maximized,
            },
        };
        match ShellApp::open_window(app, spec, cx) {
            Ok(key) => opened.push(key),
            Err(refusal) => tracing::warn!(
                target: "geode::session",
                "the window recorded on workspace {} was not reopened: {refusal:?}",
                record.workspace
            ),
        }
    }
    if opened.is_empty() {
        let initial = app.read(cx).services().workspaces.initial_index();
        let workspace = WorkspaceIx::new(initial).expect("the initial index is 1–9");
        let spec = WindowSpec {
            workspace,
            placement: Placement::Default,
        };
        let key = ShellApp::open_window(app, spec, cx)
            .expect("the first window could not be opened; nothing to run");
        opened.push(key);
    }
    opened
}

/// A recorded frame as gpui bounds, in the logical pixels it was saved in.
fn bounds_of(record: WindowBoundsRecord) -> Bounds<Pixels> {
    Bounds::new(
        point(px(record.x), px(record.y)),
        size(px(record.w), px(record.h)),
    )
}
