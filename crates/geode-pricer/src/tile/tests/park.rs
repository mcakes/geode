//! `TileContent::park`: the shell's hook before it serialises a tile and
//! drops its occupant because the workspace is moving to another window
//! or its window is closing. The sheet's write-behind save waits on an
//! idle timer; park queues it now, through the trait object the shell
//! holds, so the move never loses an edit.

use super::*;

/// A clean sheet parks without a save; a dirty one saves once, without
/// the idle wait, and the idle timer does not save it again.
#[gpui::test]
fn park_queues_the_dirty_sheet_save(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let base = h.store.save_count();
    vcx.update(|_, cx| h.content.park(cx));
    assert_eq!(
        h.store.save_count(),
        base,
        "a clean sheet has nothing to flush"
    );
    edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
    assert!(
        h.tile.read_with(&vcx, |t, _| t.dirty),
        "the edit dirtied the sheet"
    );
    vcx.update(|_, cx| h.content.park(cx));
    assert_eq!(
        h.store.save_count(),
        base + 1,
        "park saved without the idle wait"
    );
    assert!(
        !h.tile.read_with(&vcx, |t, _| t.dirty),
        "the queued save cleared dirty"
    );
    assert_eq!(stored(&h).qty(0), 2);
    settle(&mut vcx, SAVE_IDLE);
    assert_eq!(
        h.store.save_count(),
        base + 1,
        "the idle save is not repeated"
    );
}
