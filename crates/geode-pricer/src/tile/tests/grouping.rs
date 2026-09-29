//! The tile follows the grouping: the chain resolved by the blotter's
//! precedence (pin, slot pin, the frame's active slot, the view's own),
//! applied in the frame observer before the tile arrives; `:group`,
//! `:group slot N`, `:unpin`; the package verbs renamed `:package` /
//! `:unpackage`; the header's chain; fold verbs and the chevron on group
//! rows; the cursor across regrouping; read-only group rows and split
//! packages; selection totals over group rows; line movement under a
//! grouping; the session round trip.

use super::*;
use geode_core::groupings::GroupingSlots;

/// [SPX 5000 C, NDX 4000 P, CS(SPX 4800 C, SPX 5200 C)].
const MIXED: [&str; 3] = [
    "SPX Z26 5000 C",
    "NDX Z26 4000 P",
    "-5 SPX Z26 4800/5200 CS",
];

/// Fill frame slots `(n, chain)`, as a groupings reload does.
fn slots(h: &Harness, vcx: &mut VisualTestContext, filled: &[(u8, &[&str])]) {
    let mut s = GroupingSlots::default();
    for (n, chain) in filled {
        s.set(*n, chain.iter().map(|c| c.to_string()).collect());
    }
    h.frame.update(vcx, |f, cx| {
        f.replace_slots(s);
        cx.notify();
    });
}

/// Activate frame slot `n` (or none) on the tile's lane, as the palette's
/// grouping picker does; the tile's observer runs on the notify.
fn activate(h: &Harness, vcx: &mut VisualTestContext, n: Option<u8>) {
    h.frame.update(vcx, |f, cx| {
        f.shared_mut().set_active_slot(n);
        cx.notify();
    });
}

fn kept(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| t.chain.kept.clone())
}

/// Put the cursor on the painted row whose tree text is `text`.
fn cursor_to(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
    let at = h
        .tree(vcx)
        .iter()
        .position(|t| t == text)
        .unwrap_or_else(|| panic!("no row '{text}' in {:?}", h.tree(vcx)));
    h.tile.update(vcx, |t, cx| {
        t.set_cursor_row(at);
        t.sync_cursor(cx);
    });
}

fn cursor_text(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
    let row = h.cursor(vcx)?.0;
    h.tree(vcx).get(row).cloned()
}

/// A frame grouping change applies before the tile arrives: the barrier
/// opened for this tile's key is answered on the same notify, and the
/// sheet is already grouped (closed groups, byte order, `NDX` first). An
/// unrelated frame notify rebuilds nothing.
#[gpui::test]
fn a_frame_grouping_applies_and_then_the_tile_arrives(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    slots(&h, &mut vcx, &[(1, &["underlying_ref"])]);
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_active_slot(Some(1));
        f.shared_mut()
            .open_flip([QueryKey(TILE)], std::time::Instant::now());
        cx.notify();
    });
    assert!(
        !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
        "the tile arrived"
    );
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"]);
    assert_eq!(kept(&h, &vcx), ["underlying_ref"]);
    let before = h.tile.read_with(&vcx, |t, _| Rc::clone(&t.model));
    h.frame.update(&mut vcx, |_, cx| cx.notify());
    assert!(
        h.tile.read_with(&vcx, |t, _| Rc::ptr_eq(&before, &t.model)),
        "an unrelated notify rebuilds nothing"
    );
    // Back to no active slot: the view's own grouping (none) — flat.
    activate(&h, &mut vcx, None);
    assert_eq!(h.tree(&vcx).len(), 3);
}

/// Pin over slot pin over the frame's active slot over the view's own
/// `grouping`, and `:unpin` returns to the frame.
#[gpui::test]
fn the_chain_resolves_pin_then_slot_then_frame_then_view(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    reload_views(
        &h,
        &mut vcx,
        "[vanilla]\ndataset = \"pricer\"\ngrouping = [\"expiry\"]\n\
         [[vanilla.columns]]\nname = \"strike\"\nkind = \"dimension\"\n",
        NamedColours::default(),
    );
    assert_eq!(kept(&h, &vcx), ["expiry"], "the view's own");
    slots(&h, &mut vcx, &[(1, &["underlying_ref"]), (2, &["strike"])]);
    activate(&h, &mut vcx, Some(1));
    assert_eq!(kept(&h, &vcx), ["underlying_ref"], "the frame's slot");
    h.command(&mut vcx, "group slot 2").unwrap();
    assert_eq!(kept(&h, &vcx), ["strike"], "a slot pin");
    h.command(&mut vcx, "group expiry, underlying_ref").unwrap();
    assert_eq!(kept(&h, &vcx), ["expiry", "underlying_ref"], "a pin");
    activate(&h, &mut vcx, None);
    assert_eq!(
        kept(&h, &vcx),
        ["expiry", "underlying_ref"],
        "a pinned tile ignores the frame"
    );
    assert!(h.header(&vcx).contains(&"pinned".to_string()));
    h.command(&mut vcx, "unpin").unwrap();
    assert_eq!(kept(&h, &vcx), ["expiry"], "unpinned: the view again");
    assert!(!h.header(&vcx).contains(&"pinned".to_string()));
    activate(&h, &mut vcx, Some(1));
    assert_eq!(kept(&h, &vcx), ["underlying_ref"], "and the frame again");
    assert_eq!(
        h.command(&mut vcx, "group slot 3"),
        Err("slot 3 is empty".to_string())
    );
    assert_eq!(
        kept(&h, &vcx),
        ["underlying_ref"],
        "a refused pin pins nothing"
    );
}

/// A level `pricer` cannot group by (the demo's `lhu`) is dropped: the
/// sheet groups by the rest and the header strikes it through, in the
/// chain's written order, beside the neutral `pinned` chip.
#[gpui::test]
fn a_dropped_level_is_struck_through_in_the_header(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("pricer-chain").is_none(),
        "no chain, no chips"
    );
    h.command(&mut vcx, "group lhu underlying_ref position_ref")
        .unwrap();
    assert_eq!(kept(&h, &vcx), ["underlying_ref", "position_ref"]);
    let header = h.header(&vcx);
    let at = header.iter().position(|t| t == "~lhu~").expect("struck");
    assert_eq!(
        header[at..at + 4],
        ["~lhu~", "underlying_ref", "position_ref", "pinned"]
    );
    h.draw(&mut vcx);
    for sel in ["pricer-chain", "pricer-chain-dropped", "pricer-pinned"] {
        assert!(vcx.debug_bounds(sel).is_some(), "{sel} painted");
    }
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"]);
}

/// `:package [count]` / `:unpackage` are the package verbs now, the keys
/// `g p` / `g u` still reach them, and `:group 2` pins a grouping by a
/// column named `2` (dropped) — it never packages two lines.
#[gpui::test]
fn package_and_unpackage_are_the_package_verbs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "NDX Z26 4000 P"]);
    h.command(&mut vcx, "group 2").unwrap();
    assert_eq!(h.sheet_len(&vcx), 3, "no package");
    assert_eq!(h.header(&vcx)[3..5], ["~2~", "pinned"]);
    h.command(&mut vcx, "unpin").unwrap();
    h.command(&mut vcx, "package 2").unwrap();
    assert_eq!(h.sheet_len(&vcx), 4, "a package row over two lines");
    h.command(&mut vcx, "unpackage").unwrap();
    assert_eq!(h.sheet_len(&vcx), 3);
    // The keys' actions.
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(h.sheet_len(&vcx), 4, "g p");
    h.dispatch(&mut vcx, "ungroup", None);
    assert_eq!(h.sheet_len(&vcx), 3, "g u");
    assert_eq!(
        h.command(&mut vcx, "ungroup"),
        Err("unknown command 'ungroup'".to_string())
    );
}

/// `:group` completes the groupable `pricer` columns and `slot`; a
/// measure is not groupable.
#[gpui::test]
fn colon_group_completes_the_groupable_columns(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_seeded(cx, &MIXED);
    let got = h.tile.read_with(&vcx, |t, _| t.completions("group ", 6));
    for want in ["underlying_ref", "expiry", "position_ref", "slot"] {
        assert!(got.contains(&want.to_string()), "{want} in {got:?}");
    }
    assert!(!got.contains(&"npv".to_string()), "{got:?}");
}

/// `space`, `z o`, `z c`, `z shift+r`, `z shift+m` on a group row act on
/// its path and leave the package expansion alone; `z c` from a line
/// inside a group closes the group and lands on it.
#[gpui::test]
fn fold_verbs_act_on_a_group_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    assert_eq!(
        h.tree(&vcx),
        ["NDX", "SPX", "SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS"],
        "space opens the group; the package stays closed"
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX"));
    h.dispatch(&mut vcx, "collapse", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "z c");
    h.dispatch(&mut vcx, "expand", None);
    assert_eq!(h.tree(&vcx).len(), 4, "z o");
    // From a line inside the group, `z c` closes the group and lands on it.
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "collapse", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"]);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX"));
    // `z shift+r` opens groups and packages; `z shift+m` closes both.
    h.dispatch(&mut vcx, "expand_all", None);
    assert_eq!(h.tree(&vcx).len(), 7, "every group and package open");
    cursor_to(&h, &mut vcx, "5 SPX Z26 5200 C");
    h.dispatch(&mut vcx, "collapse_all", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"]);
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("SPX"),
        "the nearest painted ancestor of the leg"
    );
    // A group fold never touched the package's own state.
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.expansion.ids().next().is_none()),
        "z shift+m closed the packages too"
    );
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "-5 SPX Z26 4800/5200 CS");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    h.dispatch(&mut vcx, "toggle", None);
    assert_eq!(
        h.tree(&vcx).len(),
        6,
        "reopening the group finds its package still open"
    );
}

/// A chevron click on a group row toggles that group, and the cursor
/// lands on the group row.
#[gpui::test]
fn a_chevron_click_on_a_group_row_toggles_the_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.draw(&mut vcx);
    let at = centre_of(&mut vcx, "pricer-chevron-0");
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert_eq!(h.tree(&vcx), ["NDX", "NDX Z26 4000 P", "SPX"]);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("NDX"));
    let at = centre_of(&mut vcx, "pricer-chevron-0");
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"]);
}

/// Across a regroup the cursor follows its line; when the line's new
/// node is closed it lands on the nearest painted ancestor — never on
/// whatever row slid into its old index. Group paths deeper than the new
/// chain are pruned.
#[gpui::test]
fn the_cursor_follows_its_line_across_a_regroup(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    slots(
        &h,
        &mut vcx,
        &[(1, &["underlying_ref"]), (2, &["underlying_ref", "strike"])],
    );
    cursor_to(&h, &mut vcx, "NDX Z26 4000 P");
    activate(&h, &mut vcx, Some(1));
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("NDX"),
        "its group is closed: the group row"
    );
    h.dispatch(&mut vcx, "toggle", None);
    h.motion(&mut vcx, "down", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("NDX Z26 4000 P"));
    // Deeper chain; the line's `NDX` group stays open (same path).
    activate(&h, &mut vcx, Some(2));
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("4000"),
        "NDX is open, its 4000 group closed: the nearest painted ancestor"
    );
    h.dispatch(&mut vcx, "expand", None);
    h.motion(&mut vcx, "down", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("NDX Z26 4000 P"));
    // Back to one level: the `[NDX, 4000]` path is pruned; flat again,
    // the line is a root row.
    activate(&h, &mut vcx, Some(1));
    assert!(
        h.tile.read_with(&vcx, |t, _| {
            let p: geode_core::expansion::Path = vec![Some("NDX".into()), Some("4000".into())];
            !t.group_expansion.is_open(&p)
        }),
        "pruned on regroup"
    );
    activate(&h, &mut vcx, None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("NDX Z26 4000 P"));
}
