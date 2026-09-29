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
        t.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, GridRowKind::Package { split: true, .. }))
            .map(|(i, _)| i)
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
        (0..t.model.rows.len())
            .filter(|&g| t.model.rows[g].path.is_some())
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

/// Under a grouping, sheet order is not the painted order: `shift+j` /
/// `shift+k` refuse and move nothing.
#[gpui::test]
fn line_moves_refuse_under_a_grouping(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &MIXED);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    cursor_to(&h, &mut vcx, "SPX Z26 5000 C");
    let before = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(0));
    for verb in ["move_down", "move_up"] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("lines move in the flat sheet: clear the grouping first"),
            "{verb}"
        );
    }
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(0)), before);
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
