//! The tile follows the grouping: the chain resolved by the blotter's
//! precedence (pin, slot pin, the frame's active slot, the view's own),
//! applied in the frame observer before the tile arrives; `:group`,
//! `:group slot N`, `:unpin`; the package verbs renamed `:package` /
//! `:unpackage`; the header's chain; fold verbs and the chevron on group
//! rows; the cursor across regrouping; read-only group rows and split
//! packages; selection totals over group rows; line movement under a
//! grouping; the session round trip.

use super::*;
use crate::grid::GridRowKind;
use geode_core::groupings::GroupingSlots;

/// [SPX 5000 C, NDX 4000 P, CS(SPX 4800 C, SPX 5200 C)].
const MIXED: [&str; 3] = [
    "SPX Z26 5000 C",
    "NDX Z26 4000 P",
    "-5 SPX Z26 4800/5200 CS",
];

#[gpui::test]
fn fzf_finds_closed_group_and_package_descendants_and_reveals_only_on_pick(
    cx: &mut gpui::TestAppContext,
) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref expiry").unwrap();
    h.dispatch(&mut vcx, "collapse_all", None);
    let before = h.tree(&vcx);
    let cursor = h.cursor(&vcx);
    let results = vcx.new(|_| FuzzyFind::default());
    vcx.update(|window, cx| {
        h.tile.update(cx, |tile, cx| {
            tile.start_fuzzy_find(results.downgrade(), window, cx)
        })
    });
    results.update(&mut vcx, |results, cx| {
        results.set_query("spx 5200".into(), cx)
    });
    vcx.run_until_parked();
    assert_eq!(
        h.tree(&vcx),
        before,
        "search does not expand groups or packages"
    );
    assert_eq!(h.cursor(&vcx), cursor, "search does not move the cursor");
    let picked = results.read_with(&vcx, |results, _| results.selected_item().unwrap());
    assert!(picked.path().contains("SPX"));
    // Prefer the individual leg to its package when selecting its exact shorthand.
    let leg_label = h.tile.read_with(&vcx, |tile, _| {
        (0..tile.sheet.len())
            .map(|r| tile.sheet.shorthand(r))
            .find(|s| s.contains("5200") && !s.contains('/'))
            .unwrap()
    });
    results.update(&mut vcx, |results, cx| {
        results.set_query(leg_label.clone(), cx)
    });
    vcx.run_until_parked();
    let picked = results.read_with(&vcx, |results, _| results.selected_item().unwrap());
    assert_eq!(picked.label(), leg_label);
    vcx.update(|window, cx| picked.reveal(&leg_label, window, cx))
        .unwrap();
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some(leg_label.as_str()));
    assert!(h.tree(&vcx).len() > before.len());
    assert!(
        !h.tree(&vcx).iter().any(|label| label == "NDX Z26 4000 P"),
        "unrelated groups stay closed"
    );
}

#[gpui::test]
fn fzf_keeps_native_header_columns_and_restores_tree(cx: &mut gpui::TestAppContext) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.draw(&mut vcx);
    let header = vcx.debug_bounds("pricer-header-5").unwrap();
    let column_header = vcx.debug_bounds("pricer-th-1").unwrap();
    let value = vcx.debug_bounds("pricer-cell-0-1").unwrap();
    let before = h.tree(&vcx);
    let cursor = h.cursor(&vcx);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    results.update(&mut vcx, |results, cx| results.set_query("spx".into(), cx));
    vcx.run_until_parked();
    h.draw(&mut vcx);
    h.draw(&mut vcx);
    assert_eq!(
        vcx.debug_bounds("pricer-header-5").unwrap(),
        header,
        "tile header does not move or disappear"
    );
    assert_eq!(
        vcx.debug_bounds("pricer-th-1")
            .expect("search uses the original column header renderer"),
        column_header,
        "column header geometry is preserved"
    );
    let found = vcx
        .debug_bounds("find-cell-0-1")
        .expect("numeric cell stays visible");
    assert_eq!(
        found.origin.x, value.origin.x,
        "numeric column retains its alignment"
    );
    assert_eq!(
        found.size, value.size,
        "column width and row density are retained"
    );
    assert!(
        vcx.debug_bounds("pricer-cell-0-1").is_none(),
        "only the search table paints"
    );
    assert_eq!(h.cursor(&vcx), cursor);
    results.update(&mut vcx, |results, cx| results.close(cx));
    vcx.run_until_parked();
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("fuzzy-find").is_none(),
        "dismissal works even while a view handle is retained"
    );
    assert!(vcx.debug_bounds("pricer-cell-0-1").is_some());
    assert_eq!(h.tree(&vcx), before);
    assert_eq!(h.cursor(&vcx), cursor);
}

#[gpui::test]
fn fzf_native_table_scrolls_and_pointer_picks_without_moving_the_tree(
    cx: &mut gpui::TestAppContext,
) {
    use geode_shell::fuzzyfind::{FuzzyFind, Pick};
    let lines: Vec<_> = (0..80)
        .map(|i| format!("SPX Z26 {} C", 4000 + i * 5))
        .collect();
    let lines: Vec<_> = lines.iter().map(String::as_str).collect();
    let (h, mut vcx) = open_seeded(cx, &lines);
    let cursor = h.cursor(&vcx);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    let picked = Rc::new(RefCell::new(None));
    let _subscription = vcx.update(|_, cx| {
        let picked = picked.clone();
        cx.subscribe(&results, move |results, _: &Pick, cx| {
            *picked.borrow_mut() = results
                .read(cx)
                .selected_item()
                .map(|item| item.label().to_string());
        })
    });
    results.update(&mut vcx, |results, cx| {
        results.navigate(NavCommand::Move(40), cx)
    });
    h.draw(&mut vcx);
    h.draw(&mut vcx);
    let row = vcx
        .debug_bounds("find-result-40")
        .expect("keyboard selection stays rendered");
    let viewport = vcx.debug_bounds("fuzzy-find").unwrap();
    assert!(row.top() >= viewport.top() && row.bottom() <= viewport.bottom());
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_up(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(picked.borrow().as_deref(), Some("SPX Z26 4200 C"));
    assert_eq!(
        h.cursor(&vcx),
        cursor,
        "only committing a pick may change the tree cursor"
    );
}

/// Display positions the `/` result table painted, in order.
fn find_positions(h: &Harness, vcx: &mut VisualTestContext, rows: usize) -> Vec<usize> {
    h.draw(vcx);
    (0..rows)
        .filter(|i| {
            let selector: &'static str = Box::leak(format!("find-result-{i}").into_boxed_str());
            vcx.debug_bounds(selector).is_some()
        })
        .collect()
}

/// The grid rows the find cells hold, ascending.
fn find_held(paint: &RefCell<crate::delegate::FindPaint>, rows: usize) -> Vec<usize> {
    let paint = paint.borrow();
    (0..rows).filter(|&g| paint.cells().contains(g)).collect()
}

/// Every painted measure cell of `rows` carries the grid formatter's text
/// for its row of the find's all-open index, and at least one is not blank.
fn assert_find_paints_the_formatter(
    h: &Harness,
    vcx: &VisualTestContext,
    paint: &RefCell<crate::delegate::FindPaint>,
    rows: &[usize],
) {
    let paint = paint.borrow();
    let index = Rc::clone(&paint.painter.model);
    let mut texts = 0;
    h.tile.read_with(vcx, |t, _| {
        let mut pass = CellPass::new(t.fill_source(), &index);
        for &g in rows {
            for c in 0..index.columns.len() {
                let fresh = pass.cell(g, c).map(|c| c.text.to_string());
                texts += fresh.as_ref().is_some_and(|t| !t.is_empty()) as usize;
                // The tree column sits at table column 0.
                assert_eq!(
                    paint.painter.find_painted.get(&(g, c + 1)),
                    Some(&fresh.unwrap_or_default()),
                    "find cell ({g}, {c})"
                );
            }
        }
    });
    assert!(texts > 0, "some painted cell carries text");
}

/// `/` formats the measure cells of the rows it paints and no others: the
/// open, a real wheel scroll and a narrowing to one match each fill
/// exactly the rows entering view and drop the rest, and every painted
/// cell is the grid formatter's.
#[gpui::test]
fn fzf_formats_only_the_rows_it_paints(cx: &mut gpui::TestAppContext) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let lines: Vec<_> = (0..300)
        .map(|i| format!("SPX Z26 {} C", 3000 + i * 5))
        .collect();
    let lines: Vec<_> = lines.iter().map(String::as_str).collect();
    let (h, mut vcx) = open_seeded(cx, &lines);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    vcx.run_until_parked();
    let paint = h.tile.read_with(&vcx, |t, _| t.find_paint.clone().unwrap());
    let cols = paint.borrow().painter.model.columns.len();

    let shown = find_positions(&h, &mut vcx, 300);
    assert!(
        shown.len() > 1 && shown.len() < 100,
        "one screenful: {shown:?}"
    );
    assert_eq!(
        find_held(&paint, 300),
        shown,
        "the cells hold the painted rows"
    );
    let opened = paint.borrow().fills;
    assert!(
        opened <= geode_tile::grid::FIRST_WINDOW * cols,
        "the open formats at most a first window, not 300 rows: {opened}"
    );
    assert_find_paints_the_formatter(&h, &vcx, &paint, &shown);

    let bounds = vcx.debug_bounds("fuzzy-find").expect("the table paints");
    vcx.simulate_event(gpui::ScrollWheelEvent {
        position: bounds.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-600.))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    let scrolled = find_positions(&h, &mut vcx, 300);
    assert!(scrolled.first() > shown.first(), "scrolled: {scrolled:?}");
    assert_eq!(
        find_held(&paint, 300),
        scrolled,
        "rows scrolled out are dropped"
    );
    let entered = scrolled.iter().filter(|r| !shown.contains(r)).count();
    assert_eq!(
        paint.borrow().fills - opened,
        entered * cols,
        "only the rows scrolling in are formatted"
    );
    assert_find_paints_the_formatter(&h, &vcx, &paint, &scrolled);

    // One match far down the sheet: the table never reports a one-row range.
    results.update(&mut vcx, |results, cx| results.set_query("4205".into(), cx));
    vcx.run_until_parked();
    assert_eq!(find_positions(&h, &mut vcx, 300), vec![0]);
    assert_eq!(
        find_held(&paint, 300),
        vec![241],
        "the lone match is formatted"
    );
    assert_find_paints_the_formatter(&h, &vcx, &paint, &[241]);
}

/// The find index names rollup nodes and sheet rows as `/` opened on
/// them. Once the tile installs another index (a regroup), a row entering
/// view paints blank measure cells rather than read through the stale one.
#[gpui::test]
fn fzf_paints_blank_cells_once_the_tile_reindexes(cx: &mut gpui::TestAppContext) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    vcx.run_until_parked();
    let paint = h.tile.read_with(&vcx, |t, _| t.find_paint.clone().unwrap());
    let shown = find_positions(&h, &mut vcx, 10);
    assert_find_paints_the_formatter(&h, &vcx, &paint, &shown);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    results.update(&mut vcx, |results, cx| results.set_query("spx".into(), cx));
    vcx.run_until_parked();
    // A frame drawn while the query ranked repaints the rows already held.
    paint.borrow_mut().painter.find_painted.clear();
    assert!(!find_positions(&h, &mut vcx, 10).is_empty());
    assert!(
        find_held(&paint, 10).is_empty(),
        "nothing is formatted against the stale index"
    );
    let paint = paint.borrow();
    assert!(!paint.painter.find_painted.is_empty());
    assert!(
        paint.painter.find_painted.values().all(String::is_empty),
        "the measure cells now paint blank: {:?}",
        paint.painter.find_painted
    );
}

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

/// Activate frame slot `n` (or none) on the tile's lane, as a pick in the
/// Grouping dialog or a `frame::slot_*` chord does; the tile's observer runs
/// on the notify.
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
/// `g p` / `g u` still reach them, and `:group 2` names a column `2`
/// that `pricer` cannot group by — it refuses, and never packages two
/// lines.
#[gpui::test]
fn package_and_unpackage_are_the_package_verbs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "NDX Z26 4000 P"]);
    assert!(h.command(&mut vcx, "group 2").is_err());
    assert_eq!(h.sheet_len(&vcx), 3, "no package");
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

/// `:group` completes the groupable `pricer` columns, `none` and `slot`; a
/// measure is not groupable.
#[gpui::test]
fn colon_group_completes_the_groupable_columns(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_seeded(cx, &MIXED);
    let got = h.tile.read_with(&vcx, |t, _| t.completions("group ", 6));
    for want in ["underlying_ref", "expiry", "position_ref", "none", "slot"] {
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

/// [SPX Z26 4000 P, CAL(SPX H27 5000 C, SPX Z26 5000 C), SPX H27 4000 P]:
/// under `expiry` the calendar splits across both dates.
const CALENDAR: [&str; 3] = ["SPX Z26 4000 P", "SPX Z26/H27 5000 CAL", "SPX H27 4000 P"];

/// The footer's split-package and grouping-row refusals.
const SPLIT_TEXT: &str = "split package: edit its legs";
const GROUP_TEXT: &str = "a grouping row: edit its lines";

/// A distinct npv per line (`1 + i`), so no two lines cancel and a doubled
/// line reads differently from a single one.
fn price_distinctly(h: &Harness, vcx: &mut VisualTestContext) {
    for b in h.prices() {
        h.deliver(
            vcx,
            PriceOutcome {
                key: b.key,
                tag: b.tag,
                submitted: std::time::Instant::now(),
                results: b
                    .lines
                    .iter()
                    .enumerate()
                    .map(|(i, l)| (l.id, l.revision, Ok(result(1.0 + i as f64))))
                    .collect(),
            },
        );
    }
}

fn npv_total(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
    h.tile.read_with(vcx, |t, _| {
        t.totals
            .iter()
            .find(|c| c.label.as_ref() == "npv")
            .map(|c| c.text.to_string())
    })
}

fn npv(h: &Harness, vcx: &VisualTestContext, row: usize) -> f64 {
    h.cell(vcx, row, "npv").parse().unwrap()
}

/// The grid rows painting a split package.
fn split_rows(h: &Harness, vcx: &VisualTestContext) -> Vec<usize> {
    h.tile.read_with(vcx, |t, _| {
        (0..t.model.len())
            .filter(|&g| {
                matches!(
                    t.model.kind(g),
                    Some(GridRowKind::Package { split: true, .. })
                )
            })
            .collect()
    })
}

fn cursor_to_row(h: &Harness, vcx: &mut VisualTestContext, row: usize) {
    h.tile.update(vcx, |t, cx| {
        t.set_cursor_row(row);
        t.sync_cursor(cx);
    });
}

fn set_scope(h: &Harness, vcx: &mut VisualTestContext, expr: &str) {
    let scope = geode_core::scope::Scope {
        expression: Some(geode_core::scope::parse_expr(expr).unwrap()),
        ..Default::default()
    };
    h.frame.update(vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
}

fn clipboard(vcx: &mut VisualTestContext) -> Option<String> {
    vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// A package split across expiry nodes is read-only through every key
/// route: `i` (the cell editor), `d d`, `g u` — each refuses with the
/// split footer and writes nothing. Its leg on its own stays editable.
#[gpui::test]
fn a_split_package_refuses_edits_with_the_split_footer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let splits = split_rows(&h, &vcx);
    assert_eq!(splits.len(), 2, "the calendar under both dates");
    goto_column(&h, &mut vcx, "qty");
    cursor_to_row(&h, &mut vcx, splits[1]);
    h.dispatch(&mut vcx, "edit", None);
    assert!(
        h.tile.read_with(&vcx, |t, _| t.editor.is_none()),
        "no editor"
    );
    assert_eq!(h.footer(&vcx).as_deref(), Some(SPLIT_TEXT));
    let len = h.sheet_len(&vcx);
    for verb in ["delete", "ungroup"] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(SPLIT_TEXT), "{verb}");
        assert_eq!(h.sheet_len(&vcx), len, "{verb} wrote nothing");
    }
    assert_eq!(
        h.command(&mut vcx, "unpackage"),
        Err(SPLIT_TEXT.to_string())
    );
    // Its leg alone: the editor opens.
    cursor_to_row(&h, &mut vcx, splits[1] + 1);
    h.dispatch(&mut vcx, "edit", None);
    assert!(
        h.tile.read_with(&vcx, |t, _| t.editor.is_some()),
        "a leg edits"
    );
}

/// A grouping row has no line behind it: `i`, `d d`, `g p`, `g u` and
/// `:package` refuse with the grouping-row footer, as does a `V`
/// selection reaching one; `y y` yanks its lines' shorthand.
#[gpui::test]
fn a_group_row_is_read_only_and_yanks_its_lines(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "SPX");
    let len = h.sheet_len(&vcx);
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_TEXT), "i");
    for verb in ["delete", "group", "ungroup"] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_TEXT), "{verb}");
    }
    assert_eq!(h.command(&mut vcx, "package"), Err(GROUP_TEXT.to_string()));
    assert_eq!(h.sheet_len(&vcx), len, "nothing written");
    h.dispatch(&mut vcx, "yank_row", None);
    assert_eq!(
        clipboard(&mut vcx).as_deref(),
        Some("SPX Z26 5000 C\n-5 SPX Z26 4800/5200 CS"),
        "the group's lines, in sheet order"
    );
    // A `V` selection from a line up onto the group row refuses whole.
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "up", None);
    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_TEXT), "V d");
    assert_eq!(h.sheet_len(&vcx), len);
}

/// A `V` selection spanning groups edits the lines it paints: the group
/// rows between them have no line and are passed over, not refused.
#[gpui::test]
fn a_selection_across_groups_edits_its_lines(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "NDX Z26 4000 P");
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C"); // over the SPX group row
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.footer(&vcx), None);
    let tree = h.tree(&vcx);
    for line in [
        "NDX Z26 4500 P",
        "SPX Z26 4500 C",
        "-5 SPX Z26 4800/5200 CS",
    ] {
        assert!(tree.iter().any(|t| t == line), "{line}: {tree:?}");
    }
    assert_eq!(h.mode(&mut vcx), "normal");
}

/// A closed group inside the selection is passed over: the edit writes
/// the lines the selection paints and leaves the hidden ones as they were.
#[gpui::test]
fn a_selection_over_a_closed_group_edits_only_the_visible_lines(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 3000 P", "SPX Z26 4000 P", "SPX Z26 5000 P"]);
    h.command(&mut vcx, "group strike").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "4000");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "SPX Z26 3000 P");
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 P");
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "3");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
    h.dispatch(&mut vcx, "expand_all", None);
    let tree = h.tree(&vcx);
    for line in ["3 SPX Z26 3000 P", "SPX Z26 4000 P", "3 SPX Z26 5000 P"] {
        assert!(tree.iter().any(|t| t == line), "{line}: {tree:?}");
    }
}

/// A selection holding a group row and one of its descendants totals each
/// leg once: the group row's total is its own sum, and adding the line
/// beneath it changes nothing.
#[gpui::test]
fn totals_count_a_group_row_and_its_descendant_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    price_distinctly(&h, &mut vcx);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    assert_eq!(
        h.tree(&vcx),
        ["NDX", "SPX", "SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS"]
    );
    let (group, line) = (npv(&h, &vcx, 1), npv(&h, &vcx, 2));
    assert!(line.abs() > 0.5, "fixture: the line counts");
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "up", None); // the SPX group and its line
    assert_eq!(npv_total(&h, &vcx), Some(format!("{group:.2}")));
    h.motion(&mut vcx, "up", None); // and NDX
    let ndx = npv(&h, &vcx, 0);
    assert_eq!(npv_total(&h, &vcx), Some(format!("{:.2}", group + ndx)));
}

/// A calendar split across two expiry nodes, both of its rows selected
/// with everything else: each leg counts once, so the total is the two
/// dates' sums — never the package's whole fold once per node.
#[gpui::test]
fn totals_count_a_split_package_per_node_legs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    price_distinctly(&h, &mut vcx);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let tree = h.tree(&vcx);
    let groups: Vec<usize> = h.tile.read_with(&vcx, |t, _| {
        (0..t.model.len())
            .filter(|&g| t.model.path(g).is_some())
            .collect()
    });
    assert_eq!(groups.len(), 2, "{tree:?}");
    let expected = npv(&h, &vcx, groups[0]) + npv(&h, &vcx, groups[1]);
    cursor_to_row(&h, &mut vcx, tree.len() - 1);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "top", None);
    assert_eq!(npv_total(&h, &vcx), Some(format!("{expected:.2}")));
    // One split row alone totals its own node's leg, as its row paints —
    // not the whole calendar.
    h.dispatch(&mut vcx, "escape", None);
    let split = split_rows(&h, &vcx)[0];
    let own = npv(&h, &vcx, split);
    cursor_to_row(&h, &mut vcx, split);
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(npv_total(&h, &vcx), Some(format!("{own:.2}")));
}

/// SPX and NDX lines interleaved in the sheet, so each underlying's group
/// paints lines that are not sheet neighbours.
const INTERLEAVED: [&str; 5] = [
    "SPX Z26 4000 P",
    "NDX Z26 5000 C",
    "SPX Z26 4200 P",
    "NDX Z26 5200 C",
    "SPX Z26 4400 P",
];

const GROUP_END: &str = "cannot move past the end of the group";

/// The interleaved sheet grouped by underlying, every group open.
const GROUPED: [&str; 7] = [
    "NDX",
    "NDX Z26 5000 C",
    "NDX Z26 5200 C",
    "SPX",
    "SPX Z26 4000 P",
    "SPX Z26 4200 P",
    "SPX Z26 4400 P",
];

/// Under a value grouping `shift+j`/`shift+k` move a line among its own
/// group's lines: the step hops the other group's lines between them in
/// the sheet, a count steps that many group siblings, the group's end
/// refuses, and the other group's painted order never changes. One undo
/// restores the move.
#[gpui::test]
fn a_grouped_move_steps_within_its_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &INTERLEAVED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    assert_eq!(h.tree(&vcx), GROUPED);
    cursor_to(&h, &mut vcx, "SPX Z26 4000 P");
    h.dispatch(&mut vcx, "move_down", Some(2));
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(
        h.tree(&vcx),
        [
            "NDX",
            "NDX Z26 5000 C",
            "NDX Z26 5200 C",
            "SPX",
            "SPX Z26 4200 P",
            "SPX Z26 4400 P",
            "SPX Z26 4000 P",
        ]
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 4000 P"));
    let tree = h.tree(&vcx);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_END));
    assert_eq!(h.tree(&vcx), tree, "the group's end moves nothing");
    h.dispatch(&mut vcx, "move_up", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.tree(&vcx)[5..], ["SPX Z26 4000 P", "SPX Z26 4400 P"]);
    h.dispatch(&mut vcx, "undo", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx), GROUPED, "each move is one undo step");
    // The NDX group's first line stops at its top.
    cursor_to(&h, &mut vcx, "NDX Z26 5000 C");
    h.dispatch(&mut vcx, "move_up", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_END));
}

/// A `V` block of a group's lines slides as one through the group's own
/// lines, keeping its selection; a selection reaching the other group
/// still refuses on its grouping row.
#[gpui::test]
fn a_grouped_block_move_stays_in_its_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &INTERLEAVED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "SPX Z26 4000 P");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(
        h.tree(&vcx)[3..],
        ["SPX", "SPX Z26 4400 P", "SPX Z26 4000 P", "SPX Z26 4200 P"]
    );
    assert_eq!(h.tree(&vcx)[..3], GROUPED[..3], "NDX unchanged");
    assert!(h.tile.read_with(&vcx, |t, _| t.selection.is_some()));
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_END));
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx), GROUPED);
}

/// A leg moves among its package's legs under a grouping, stopping at the
/// package's end with the flat wording; a split package and a leg of one
/// refuse with the split footer and move nothing.
#[gpui::test]
fn grouped_leg_moves_stay_in_the_package_and_split_ones_refuse(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let legs: Vec<usize> = h.tile.read_with(&vcx, |t, _| {
        (0..t.model.len())
            .filter(|&g| matches!(t.model.kind(g), Some(GridRowKind::Leg { .. })))
            .collect()
    });
    assert_eq!(legs.len(), 2);
    let first = h.tree(&vcx)[legs[0]].clone();
    cursor_to_row(&h, &mut vcx, legs[0]);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.tree(&vcx)[legs[1]], first, "the legs swapped");
    assert_eq!(cursor_text(&h, &vcx), Some(first));
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("cannot move past the end"));

    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let before = h.tree(&vcx);
    let splits = split_rows(&h, &vcx);
    for row in [splits[0], splits[0] + 1] {
        cursor_to_row(&h, &mut vcx, row);
        for verb in ["move_down", "move_up"] {
            h.dispatch(&mut vcx, verb, None);
            assert_eq!(
                h.footer(&vcx).as_deref(),
                Some(SPLIT_TEXT),
                "{verb} at {row}"
            );
        }
    }
    assert_eq!(h.tree(&vcx), before);
}

/// With no grouping, a move whose neighbour the scope hides steps past it
/// to the next shown sibling: the painted order changes, as the key says.
#[gpui::test]
fn a_move_steps_past_a_sibling_the_scope_hides(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 5200 C"]);
    set_scope(&h, &mut vcx, "strike != 4000");
    assert_eq!(h.tree(&vcx), ["SPX Z26 5000 C", "SPX Z26 5200 C"]);
    // A selection slides past the hidden sibling to the next shown one.
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(
        h.tree(&vcx),
        ["SPX Z26 5200 C", "SPX Z26 5000 C"],
        "V shift+j"
    );
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx), ["SPX Z26 5000 C", "SPX Z26 5200 C"]);
    // So does a single line.
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.tree(&vcx), ["SPX Z26 5200 C", "SPX Z26 5000 C"]);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 5000 C"));
    h.dispatch(&mut vcx, "move_up", None);
    assert_eq!(h.tree(&vcx), ["SPX Z26 5000 C", "SPX Z26 5200 C"]);
}

/// The pin and the open grouping rows ride the session record: a tile
/// restored from it groups the same way with the same rows open (a
/// nested open path included), and a record saved mid-load keeps them.
#[gpui::test]
fn the_pin_and_the_open_groups_round_trip_the_session(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref strike").unwrap();
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "5000");
    h.dispatch(&mut vcx, "toggle", None);
    let before = h.tree(&vcx);
    let saved = h.serialize(&mut vcx);
    let r = crate::session::Record::from_table(&saved);
    assert_eq!(
        r.pinned,
        Some(vec!["underlying_ref".to_string(), "strike".to_string()])
    );
    assert_eq!(r.pinned_slot, None);
    assert_eq!(
        r.expanded_paths,
        vec![
            vec![Some("SPX".to_string())],
            vec![Some("SPX".to_string()), Some("5000".to_string())]
        ]
    );
    // Restored through the factory, from the same store.
    let (r2, mut vcx2) = open_full(
        cx,
        Some(saved.clone()),
        h.store.clone(),
        PricerSettings::default(),
    );
    assert_eq!(r2.tree(&vcx2), before, "grouped and opened as it was");
    assert!(r2.header(&vcx2).contains(&"pinned".to_string()));
    // Mid-load, the record keeps what it was restored with.
    let rows = h.store.get("book").unwrap();
    h.store.set_pending(true);
    let (r3, mut vcx3) = open_full(cx, Some(saved), h.store.clone(), PricerSettings::default());
    let mid = crate::session::Record::from_table(&r3.serialize(&mut vcx3));
    assert_eq!(mid.expanded_paths, r.expanded_paths, "held while loading");
    r3.tile
        .update(&mut vcx3, |t, cx| t.loaded(Ok(Some(rows)), cx));
    assert_eq!(r3.tree(&vcx3), before);
    // A slot pin is its own key.
    slots(&r2, &mut vcx2, &[(1, &["expiry"])]);
    r2.command(&mut vcx2, "group slot 1").unwrap();
    let r = crate::session::Record::from_table(&r2.serialize(&mut vcx2));
    assert_eq!((r.pinned, r.pinned_slot), (None, Some(1)));
}

/// A package split across nodes paints once per node, with one id: `j`
/// walks every row, the second split row included, instead of snapping
/// back to the first row that id names.
#[gpui::test]
fn motions_walk_past_a_split_packages_second_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let rows = h.tree(&vcx).len();
    h.motion(&mut vcx, "top", None);
    let mut seen = vec![h.cursor(&vcx).unwrap().0];
    for _ in 1..rows {
        h.motion(&mut vcx, "down", None);
        seen.push(h.cursor(&vcx).unwrap().0);
    }
    assert_eq!(seen, (0..rows).collect::<Vec<_>>());
    assert_eq!(split_rows(&h, &vcx).len(), 2, "fixture: split twice");
}

/// A `V` anchored on a split package's SECOND painted row stays there:
/// the selection is that row alone and totals its node's leg, not the
/// other node's half (the anchor carries its group path).
#[gpui::test]
fn a_selection_anchored_on_a_split_packages_second_row_stays_there(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    price_distinctly(&h, &mut vcx);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let second = split_rows(&h, &vcx)[1];
    let own = npv(&h, &vcx, second);
    cursor_to_row(&h, &mut vcx, second);
    h.dispatch(&mut vcx, "visual_rows", None);
    let rows = h
        .tile
        .read_with(&vcx, |t, _| t.resolved().map(|r| r.rows.clone()));
    assert_eq!(rows, Some(second..second + 1));
    assert_eq!(npv_total(&h, &vcx), Some(format!("{own:.2}")));
    // Extending down one row keeps the anchor where it was.
    h.motion(&mut vcx, "down", None);
    let rows = h
        .tile
        .read_with(&vcx, |t, _| t.resolved().map(|r| r.rows.clone()));
    assert_eq!(rows, Some(second..second + 2));
}

/// A counted `g p` under a value grouping refuses and packages nothing:
/// its run is in sheet order, which the grouping does not paint.
#[gpui::test]
fn a_counted_package_refuses_under_a_value_grouping(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "NDX Z26 4000 P", "SPX Z26 4000 P"]);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    let len = h.sheet_len(&vcx);
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(h.footer(&vcx).as_deref(), Some(PACKAGE_GROUPED));
    assert_eq!(
        h.command(&mut vcx, "package 2"),
        Err(PACKAGE_GROUPED.to_string())
    );
    assert_eq!(h.sheet_len(&vcx), len, "nothing packaged");
    // A single line packages alone: nothing in sheet order is swept in.
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(h.sheet_len(&vcx), len + 1);
}

/// A chain of structural levels alone paints in sheet order: moves and a
/// counted `g p` work as in the flat sheet.
#[gpui::test]
fn a_structural_chain_moves_and_packages_as_the_flat_sheet(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "NDX Z26 4000 P", "SPX Z26 4000 P"]);
    h.command(&mut vcx, "group position_ref").unwrap();
    assert_eq!(kept(&h, &vcx), ["position_ref"]);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.tree(&vcx)[..2], ["NDX Z26 4000 P", "SPX Z26 5000 C"]);
    h.motion(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.sheet_len(&vcx), 4, "a package over two lines");
}

/// `p` and `shift+p` on a grouping row put at the end of the sheet and
/// say so; `o` there opens the bar labelled `at end`.
#[gpui::test]
fn put_and_add_on_a_group_row_go_to_the_end(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "NDX");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "NDX Z26 4000 P");
    h.dispatch(&mut vcx, "yank_row", None);
    let len = h.sheet_len(&vcx);
    for verb in ["put_below", "put_above"] {
        cursor_to(&h, &mut vcx, "SPX");
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(PUT_AT_END), "{verb}");
        let last = h
            .tile
            .read_with(&vcx, |t, _| t.sheet.shorthand(t.sheet.len() - 1));
        assert_eq!(last, "NDX Z26 4000 P", "{verb} lands last");
    }
    assert_eq!(h.sheet_len(&vcx), len + 2);
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "add_below", None);
    assert_eq!(h.entry_label(&vcx).as_deref(), Some("at end"));
}

/// A line typed into the entry bar that lands in a closed group opens
/// that group, and the cursor rests on the line — it never vanishes into
/// a closed group.
#[gpui::test]
fn an_entered_line_opens_its_closed_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "fixture: every group closed");
    cursor_to(&h, &mut vcx, "NDX");
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "SPX Z26 3000 P");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        h.tree(&vcx),
        [
            "NDX",
            "SPX",
            "SPX Z26 5000 C",
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 3000 P"
        ],
        "its SPX group opened; NDX stays closed"
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 3000 P"));
}

/// `p` whose lines land in a closed group opens it and rests on what
/// landed.
#[gpui::test]
fn a_put_into_a_closed_group_opens_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "NDX");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "NDX Z26 4000 P");
    h.dispatch(&mut vcx, "yank_row", None);
    cursor_to(&h, &mut vcx, "NDX");
    h.dispatch(&mut vcx, "toggle", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "fixture: NDX closed again");
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "put_below", None);
    assert_eq!(
        h.tree(&vcx),
        ["NDX", "NDX Z26 4000 P", "NDX Z26 4000 P", "SPX"],
        "the put line's NDX group opened"
    );
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2), "on the put line");
}

/// An undo restoring a line into a closed group opens the group and
/// rests on the restored line.
#[gpui::test]
fn an_undo_restoring_into_a_closed_group_opens_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "delete", None);
    cursor_to(&h, &mut vcx, "SPX");
    h.dispatch(&mut vcx, "toggle", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "fixture: SPX closed");
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.tree(&vcx),
        ["NDX", "SPX", "SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS"]
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 5000 C"));
}

/// [SPX 5000 C, SPX 5000 P, SPX 4000 P]: under `strike` the 5000 group
/// outlives one of its lines moving to 4000.
const STRIKES: [&str; 3] = ["SPX Z26 5000 C", "SPX Z26 5000 P", "SPX Z26 4000 P"];

/// An edit of the grouped column moves the line to another group: the
/// cursor follows the line there, not onto its old group's row.
#[gpui::test]
fn an_edit_of_the_grouped_value_keeps_the_cursor_on_its_line(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &STRIKES);
    h.command(&mut vcx, "group strike").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4000");
    h.dispatch(&mut vcx, "commit", None);
    assert!(
        h.tree(&vcx).contains(&"5000".to_string()),
        "fixture: 5000 stays"
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 4000 C"));
}

/// A `V` edit of the grouped column ends visual mode with the cursor on
/// the edited line, in its new group.
#[gpui::test]
fn a_selection_edit_of_the_grouped_value_follows_the_line(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &STRIKES);
    h.command(&mut vcx, "group strike").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4000");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.footer(&vcx), None, "no lost anchor");
    assert_eq!(h.mode(&mut vcx), "normal");
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 4000 C"));
    assert!(h.tile.read_with(&vcx, |t, _| t.resolved().is_none()));
}

fn find(h: &Harness, vcx: &mut VisualTestContext, e: FindEvent) {
    vcx.update(|window, cx| h.content.find(e, window, cx));
}

/// `/` reaches lines inside closed groups: a match opens its groups and
/// the cursor rests on it; `n`/`N` walk the rollup as it paints with
/// every group open; a closed package is found by its legs.
#[gpui::test]
fn find_opens_the_groups_of_a_match_inside_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "fixture: every group closed");
    h.motion(&mut vcx, "top", None);
    find(&h, &mut vcx, FindEvent::Changed("z26".into()));
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("NDX Z26 4000 P"));
    assert_eq!(h.tree(&vcx), ["NDX", "NDX Z26 4000 P", "SPX"]);
    find(&h, &mut vcx, FindEvent::Committed("z26".into()));
    h.dispatch(&mut vcx, "find_next", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 5000 C"));
    h.dispatch(&mut vcx, "find_next", None);
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("-5 SPX Z26 4800/5200 CS")
    );
    h.dispatch(&mut vcx, "find_next", None);
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("NDX Z26 4000 P"),
        "wraps in painted order"
    );
    h.dispatch(&mut vcx, "find_prev", None);
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("-5 SPX Z26 4800/5200 CS")
    );
    // Two levels deep, all closed: a closed package is found by a leg's
    // strike, and both its groups open.
    h.command(&mut vcx, "group underlying_ref expiry").unwrap();
    h.dispatch(&mut vcx, "collapse_all", None);
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "fixture: all closed");
    h.motion(&mut vcx, "top", None);
    find(&h, &mut vcx, FindEvent::Changed("5200".into()));
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("-5 SPX Z26 4800/5200 CS"),
        "{:?}",
        h.tree(&vcx)
    );
}

/// `:group` whose every level `pricer` would drop refuses and pins
/// nothing: `:group 2` is not a package verb in disguise.
#[gpui::test]
fn a_group_with_no_groupable_level_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    for (line, cols) in [("group 2", "2"), ("group nosuchcol lhu", "nosuchcol lhu")] {
        assert_eq!(
            h.command(&mut vcx, line),
            Err(format!(
                "no groupable column in {cols}; :package N packages lines"
            )),
            "{line}"
        );
        assert!(kept(&h, &vcx).is_empty(), "{line}: nothing pinned");
        assert!(!h.header(&vcx).contains(&"pinned".to_string()), "{line}");
        assert_eq!(h.tree(&vcx).len(), 3, "{line}: still flat");
    }
}

/// `:group none` pins the empty chain: the flat sheet under a grouped
/// frame, with no "no groupable column" refusal. The header shows the
/// `pinned` chip and a muted `ungrouped` where the chain would be (no
/// chain chips); moves and a counted `g p` work as in the flat sheet; a
/// frame slot change does not regroup it; `:unpin` follows the frame at
/// once.
#[gpui::test]
fn group_none_pins_the_flat_sheet_until_unpin(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    slots(&h, &mut vcx, &[(1, &["underlying_ref"]), (2, &["expiry"])]);
    activate(&h, &mut vcx, Some(1));
    assert_eq!(h.tree(&vcx), ["NDX", "SPX"], "grouped by the frame");

    h.command(&mut vcx, "group none").unwrap();
    assert!(kept(&h, &vcx).is_empty());
    assert_eq!(h.tree(&vcx).len(), 3, "the flat sheet");
    let header = h.header(&vcx);
    for want in ["pinned", "ungrouped"] {
        assert!(header.contains(&want.to_string()), "{want} in {header:?}");
    }
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("pricer-chain").is_none(), "no chain chips");
    assert!(vcx.debug_bounds("pricer-ungrouped").is_some());
    assert!(vcx.debug_bounds("pricer-pinned").is_some());

    // Moves and a counted `g p` work as in the flat sheet.
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.tree(&vcx)[..2], ["NDX Z26 4000 P", "SPX Z26 5000 C"]);
    h.motion(&mut vcx, "top", None);
    let len = h.sheet_len(&vcx);
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(h.sheet_len(&vcx), len + 1, "a package over two lines");

    activate(&h, &mut vcx, Some(2));
    assert!(kept(&h, &vcx).is_empty(), "the frame does not regroup it");
    assert!(h.header(&vcx).contains(&"ungrouped".to_string()));

    h.command(&mut vcx, "unpin").unwrap();
    assert_eq!(kept(&h, &vcx), ["expiry"], "the frame's slot at once");
    let header = h.header(&vcx);
    for gone in ["pinned", "ungrouped"] {
        assert!(!header.contains(&gone.to_string()), "{gone} in {header:?}");
    }
}

/// A `none` pin is saved as an empty `pinned` array and the factory's
/// restore reads it back as the empty pin, not as "no pin": the restored
/// tile stays flat under a grouped frame.
#[gpui::test]
fn a_group_none_pin_round_trips_the_session(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group none").unwrap();
    let saved = h.serialize(&mut vcx);
    let r = crate::session::Record::from_table(&saved);
    assert_eq!((r.pinned, r.pinned_slot), (Some(Vec::new()), None));

    let (r2, mut vcx2) = open_full(cx, Some(saved), h.store.clone(), PricerSettings::default());
    slots(&r2, &mut vcx2, &[(1, &["underlying_ref"])]);
    activate(&r2, &mut vcx2, Some(1));
    assert!(kept(&r2, &vcx2).is_empty(), "restored pinned to none");
    assert_eq!(r2.tree(&vcx2).len(), 3);
    assert!(r2.header(&vcx2).contains(&"pinned".to_string()));
}

/// On a split package row, `y y` and `V y` yank what the row shows: its
/// legs under that node, not the whole package.
#[gpui::test]
fn a_yank_on_a_split_package_row_yanks_its_nodes_legs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let register = |h: &Harness, vcx: &VisualTestContext| {
        h.tile.read_with(vcx, |t, _| {
            t.register.as_ref().map(|r| {
                r.iter()
                    .map(|s| matches!(s, RowSpec::Package { .. }))
                    .collect::<Vec<_>>()
            })
        })
    };
    for split in split_rows(&h, &vcx) {
        let leg = h.tree(&vcx)[split + 1].clone();
        cursor_to_row(&h, &mut vcx, split);
        h.dispatch(&mut vcx, "yank_row", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some(leg.as_str()), "y y");
        assert_eq!(register(&h, &vcx), Some(vec![false]), "y y: one line");
        h.tile.update(&mut vcx, |t, _| t.register = None);
        cursor_to_row(&h, &mut vcx, split);
        h.dispatch(&mut vcx, "visual_rows", None);
        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some(leg.as_str()), "V y");
        assert_eq!(register(&h, &vcx), Some(vec![false]), "V y: one line");
    }
}

/// An editor whose line a regroup puts inside a closed group is dropped
/// with a footer that says so, not the generic "the cell moved".
#[gpui::test]
fn an_editor_a_regroup_hides_says_its_line_moved_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    slots(&h, &mut vcx, &[(1, &["underlying_ref"])]);
    cursor_to(&h, &mut vcx, "NDX Z26 4000 P");
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.editor.is_some()), "fixture");
    activate(&h, &mut vcx, Some(1));
    assert!(h.tile.read_with(&vcx, |t, _| t.editor.is_none()), "dropped");
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("the line moved to another group; edit dropped")
    );
}

/// A split package's cursor keeps its node across a regroup to a shorter
/// chain: on the calendar's `H27 · 5000` row, a regroup to `expiry` alone
/// lands on its `H27` row, not on the `H27` group row nor its `Z26` half.
#[gpui::test]
fn a_split_rows_cursor_keeps_its_node_across_a_shorter_regroup(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &CALENDAR);
    h.command(&mut vcx, "group expiry strike").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let splits = split_rows(&h, &vcx);
    assert_eq!(splits.len(), 2, "fixture: split under both dates");
    cursor_to_row(&h, &mut vcx, splits[1]);
    h.command(&mut vcx, "group expiry").unwrap();
    let splits = split_rows(&h, &vcx);
    assert_eq!(splits.len(), 2, "still split under both dates");
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(splits[1]));
}

/// A price-only delivery keeps the index `/` opened on and refills the rows
/// it shows from the new prices before the next paint: no row keeps the
/// price it entered view with beside rows reading the new one.
#[gpui::test]
fn fzf_a_price_refresh_repaints_the_shown_rows(cx: &mut gpui::TestAppContext) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let (h, mut vcx) = open_seeded(cx, &["1 SPX Z26 5000 C", "2 SPX Z26 4000 P"]);
    let first = h.prices().pop().expect("a batch");
    h.answer(&mut vcx, &first, 12.5);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    vcx.run_until_parked();
    let paint = h.tile.read_with(&vcx, |t, _| t.find_paint.clone().unwrap());
    let npv = 1 + paint
        .borrow()
        .painter
        .model
        .columns
        .iter()
        .position(|c| c.name == "npv")
        .expect("an npv column");
    let shown = find_positions(&h, &mut vcx, 2);
    assert_eq!(shown, vec![0, 1]);
    let npv_painted = |g: usize| {
        paint
            .borrow()
            .painter
            .find_painted
            .get(&(g, npv))
            .cloned()
            .unwrap_or_default()
    };
    assert!(npv_painted(0).contains("12"), "{}", npv_painted(0));

    h.dispatch(&mut vcx, "price", None);
    let batch = h.prices().pop().expect("a reprice");
    let builds = crate::grid::builds();
    h.answer(&mut vcx, &batch, 99.25);
    assert_eq!(crate::grid::builds(), builds, "a price-only refresh");
    paint.borrow_mut().painter.find_painted.clear();
    assert_eq!(find_positions(&h, &mut vcx, 2), shown);
    for g in shown.iter().copied() {
        assert!(npv_painted(g).contains("99"), "row {g}: {}", npv_painted(g));
    }
    assert_find_paints_the_formatter(&h, &vcx, &paint, &shown);
}

/// Once the tile installs another index the find's measure cells paint
/// blank; the status line says the results are out of date, so a blank
/// does not read as an unpriced line.
#[gpui::test]
fn fzf_a_reindex_says_the_results_are_out_of_date(cx: &mut gpui::TestAppContext) {
    use geode_shell::fuzzyfind::FuzzyFind;
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    let results = vcx.new(|_| FuzzyFind::default());
    h.tile.update_in(&mut vcx, |tile, window, cx| {
        tile.start_fuzzy_find(results.downgrade(), window, cx)
    });
    vcx.run_until_parked();
    h.draw(&mut vcx);
    let status = |vcx: &VisualTestContext| results.read_with(vcx, |r, _| r.context().1);
    assert_eq!(status(&vcx), "5 matches");
    h.command(&mut vcx, "group underlying_ref").unwrap();
    assert_eq!(status(&vcx), FIND_OUT_OF_DATE);
    results.update(&mut vcx, |results, cx| results.set_query("spx".into(), cx));
    vcx.run_until_parked();
    assert_eq!(status(&vcx), FIND_OUT_OF_DATE, "it outlasts a new query");
}
