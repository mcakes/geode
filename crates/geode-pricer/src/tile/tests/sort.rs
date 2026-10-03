//! The column sort, through its production routes: `s`/`shift+s` matched
//! by the keymap and dispatched through the shell's door, a click on a
//! header's painted sort icon, `:sort` and its completions; the drop
//! notice; the sheet-order verbs it refuses; an insert landing where it
//! sorts; a live price refresh re-ranking; find and yank in painted order.

use super::*;
use crate::core::sort::SortSpec;
use geode_core::sort::SortOrder;
use geode_shell::keymap::{MatchResult, Matcher, build_keymap, parse_keystroke};

/// Flat sheet rows: 0 A, 1 CS (legs 2, 3), 4 P, 5 N. Strikes 5000,
/// 4800/5200, 4000, 3000.
const LINES: [&str; 4] = [
    "SPX Z26 5000 C",
    "-5 SPX Z26 4800/5200 CS",
    "SPX Z26 4000 P",
    "NDX Z26 3000 C",
];

/// Plan columns under the builtin `vanilla` view.
const STRIKE: usize = 3;
const NPV: usize = 8;

fn sort(h: &Harness, vcx: &VisualTestContext) -> Option<(&'static str, SortOrder)> {
    h.tile
        .read_with(vcx, |t, _| t.sort.map(|s| (s.column, s.order)))
}

/// A real key: matched against the module's keymap fragment under the
/// tile's own key context, then dispatched through the shell's door.
fn press(h: &Harness, vcx: &mut VisualTestContext, key: &str) {
    let mut registry = geode_shell::actions::ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    h.factory.register_actions(&mut registry);
    let doc =
        geode_shell::keymap::fragments::fragment_doc("pricer", crate::content::DEFAULT_KEYMAP)
            .unwrap();
    let (keymap, diags) = build_keymap(&[doc], geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let stack = [h.tile.read_with(vcx, |t, _| t.key_context())];
    let ks = parse_keystroke(key, geode_shell::defaults::default_mod()).unwrap();
    let MatchResult::Matched { action, count } = Matcher::default().press(&keymap, ks, &stack)
    else {
        panic!("{key}: no binding in normal mode");
    };
    assert!(vcx.update(|window, cx| h.content.dispatch(&action, count, window, cx)));
    h.draw(vcx);
}

/// Answer every pending batch, each line at `price(its shorthand)`.
fn answer_by(h: &Harness, vcx: &mut VisualTestContext, price: impl Fn(&str) -> f64) {
    let batches = h.prices();
    assert!(!batches.is_empty(), "fixture: a price batch is pending");
    for params in batches {
        let results = params
            .lines
            .iter()
            .map(|l| {
                let text = h.tile.read_with(vcx, |t, _| {
                    t.sheet
                        .index_of(crate::core::LineId(l.id))
                        .map(|r| t.sheet.shorthand(r))
                        .unwrap_or_default()
                });
                (l.id, l.revision, Ok(result(price(&text))))
            })
            .collect();
        h.deliver(
            vcx,
            PriceOutcome {
                key: params.key,
                tag: params.tag,
                submitted: std::time::Instant::now(),
                results,
            },
        );
    }
}

fn cursor_text(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
    let row = h.cursor(vcx)?.0;
    h.tree(vcx).get(row).cloned()
}

fn sheet_order(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| {
        t.sheet.roots().map(|r| t.sheet.shorthand(r)).collect()
    })
}

/// The table column's header name (`npv |x|` under an absolute sort) and
/// the arrow `column()` reports for it.
fn header_of(
    h: &Harness,
    vcx: &VisualTestContext,
    table_col: usize,
) -> (String, Option<gpui_component::table::ColumnSort>) {
    use gpui_component::table::TableDelegate as _;
    h.tile.read_with(vcx, |t, cx| {
        let c = t.table.read(cx).delegate().column(table_col, cx);
        (c.name.to_string(), c.sort)
    })
}

#[gpui::test]
fn s_and_shift_s_from_the_keymap_cycle_the_cursor_column(cx: &mut gpui::TestAppContext) {
    use gpui_component::table::ColumnSort;
    let (h, mut vcx) = open_seeded(cx, &LINES);
    let unsorted = h.tree(&vcx);
    h.motion(&mut vcx, "right", Some(STRIKE as u32));
    press(&h, &mut vcx, "s");
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Asc)));
    assert_eq!(
        h.tree(&vcx),
        vec![
            "NDX Z26 3000 C",
            "SPX Z26 4000 P",
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 5000 C",
        ],
        "strikes ascending; the package by its first leg's"
    );
    assert_eq!(
        header_of(&h, &vcx, STRIKE + 1).1,
        Some(ColumnSort::Ascending)
    );
    press(&h, &mut vcx, "s");
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
    assert_eq!(h.tree(&vcx)[0], "SPX Z26 5000 C");
    press(&h, &mut vcx, "s");
    assert_eq!(sort(&h, &vcx), None);
    assert_eq!(h.tree(&vcx), unsorted, "clear restores sheet order exactly");
    // A strike has no magnitude: `shift+s` leaves it alone.
    press(&h, &mut vcx, "shift+s");
    assert_eq!(sort(&h, &vcx), None);

    h.motion(&mut vcx, "right", Some((NPV - STRIKE) as u32));
    press(&h, &mut vcx, "shift+s");
    assert_eq!(sort(&h, &vcx), Some(("npv", SortOrder::AbsDesc)));
    assert_eq!(
        header_of(&h, &vcx, NPV + 1),
        ("npv |x|".to_string(), Some(ColumnSort::Descending)),
        "the label says the order is absolute"
    );
    press(&h, &mut vcx, "shift+s");
    assert_eq!(sort(&h, &vcx), Some(("npv", SortOrder::AbsAsc)));
    press(&h, &mut vcx, "s");
    assert_eq!(
        sort(&h, &vcx),
        Some(("npv", SortOrder::Asc)),
        "s restarts its own cycle"
    );
    press(&h, &mut vcx, "shift+s");
    press(&h, &mut vcx, "shift+s");
    press(&h, &mut vcx, "shift+s");
    assert_eq!(sort(&h, &vcx), None);
    assert_eq!(header_of(&h, &vcx, NPV + 1).0, "npv");
    assert_eq!(
        h.serialize(&mut vcx).get("sort"),
        None,
        "the sort is never saved"
    );
}

/// The painted sort icon beside a header's label: the label fills the
/// header up to it.
fn click_sort_icon(h: &Harness, vcx: &mut VisualTestContext, table_col: usize) {
    h.draw(vcx);
    let selector: &'static str = Box::leak(format!("pricer-th-{table_col}").into_boxed_str());
    let label = vcx.debug_bounds(selector).expect("the header is painted");
    let at = gpui::point(label.right() + gpui::px(8.), label.center().y);
    click_at(vcx, at, 1);
    vcx.run_until_parked();
    h.draw(vcx);
}

#[gpui::test]
fn a_header_click_walks_the_click_cycle_and_the_tree_column_never_sorts(
    cx: &mut gpui::TestAppContext,
) {
    use gpui_component::table::{ColumnSort, TableDelegate as _};
    let (h, mut vcx) = open_seeded(cx, &LINES);
    let cursor = cursor_text(&h, &vcx);
    let mut seen = Vec::new();
    for _ in 0..3 {
        click_sort_icon(&h, &mut vcx, STRIKE + 1);
        seen.push(sort(&h, &vcx));
    }
    assert_eq!(
        seen,
        vec![
            Some(("strike", SortOrder::Desc)),
            Some(("strike", SortOrder::Asc)),
            None
        ],
        "a dimension: desc, asc, clear"
    );
    let mut seen = Vec::new();
    for _ in 0..5 {
        click_sort_icon(&h, &mut vcx, NPV + 1);
        seen.push(sort(&h, &vcx).map(|s| s.1));
    }
    assert_eq!(
        seen,
        vec![
            Some(SortOrder::Desc),
            Some(SortOrder::Asc),
            Some(SortOrder::AbsDesc),
            Some(SortOrder::AbsAsc),
            None
        ],
        "a measure walks the absolute pair too"
    );
    // Another column's click starts its own cycle at desc.
    click_sort_icon(&h, &mut vcx, NPV + 1);
    click_sort_icon(&h, &mut vcx, STRIKE + 1);
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
    assert_eq!(
        cursor_text(&h, &vcx),
        cursor,
        "the cursor stays on its line"
    );

    // The tree column offers no icon, and its hook refuses a direct call.
    assert_eq!(header_of(&h, &vcx, 0).1, None);
    vcx.update(|window, cx| {
        h.tile.read(cx).table.clone().update(cx, |t, cx| {
            t.delegate_mut()
                .perform_sort(0, ColumnSort::Descending, window, cx)
        })
    });
    vcx.run_until_parked();
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
}

#[gpui::test]
fn sort_commands_resolve_against_the_plan_and_complete(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &LINES);
    let unsorted = h.tree(&vcx);
    h.command(&mut vcx, "sort strike desc").unwrap();
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
    // A column with no magnitude: `abs` is its signed direction.
    h.command(&mut vcx, "sort strike abs asc").unwrap();
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Asc)));
    h.command(&mut vcx, "sort npv abs").unwrap();
    assert_eq!(sort(&h, &vcx), Some(("npv", SortOrder::AbsDesc)));
    assert_eq!(
        h.command(&mut vcx, "sort barrier"),
        Err("no column named 'barrier' in this view".into()),
        "a vocabulary column the view does not show"
    );
    assert_eq!(sort(&h, &vcx), Some(("npv", SortOrder::AbsDesc)));
    h.command(&mut vcx, "sort clear").unwrap();
    assert_eq!(sort(&h, &vcx), None);
    assert_eq!(h.tree(&vcx), unsorted);
    let words = |vcx: &mut VisualTestContext, line: &str| {
        vcx.update(|_, cx| h.content.completions(line, line.len(), cx))
    };
    let columns = words(&mut vcx, "sort ");
    assert!(columns.contains(&"strike".to_string()), "{columns:?}");
    assert!(columns.contains(&"npv".to_string()) && columns.contains(&"clear".to_string()));
    assert!(
        !columns.contains(&"barrier".to_string()),
        "only the plan's columns"
    );
    assert_eq!(words(&mut vcx, "sort npv "), vec!["abs", "asc", "desc"]);
    assert_eq!(words(&mut vcx, "sort npv abs "), vec!["asc", "desc"]);
}

#[gpui::test]
fn a_view_without_the_sorted_column_drops_the_sort_and_says_which(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &LINES);
    h.command(&mut vcx, "view barrier").unwrap();
    h.command(&mut vcx, "sort strike desc").unwrap();
    let sorted = h.tree(&vcx);
    // A column move keeps the sort: it names the column, not its slot.
    vcx.update(|window, cx| {
        h.tile.read(cx).table.clone().update(cx, |t, cx| {
            use gpui_component::table::TableDelegate as _;
            t.delegate_mut().move_column(STRIKE + 1, 1, window, cx)
        })
    });
    vcx.run_until_parked();
    assert_eq!(h.columns(&vcx)[0], "strike", "fixture: the column moved");
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
    assert_eq!(h.tree(&vcx), sorted);

    h.command(&mut vcx, "sort barrier").unwrap();
    h.command(&mut vcx, "view vanilla").unwrap();
    assert_eq!(sort(&h, &vcx), None, "vanilla has no barrier column");
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("sort on 'barrier' dropped: the column is no longer in this view")
    );
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.header.notice.as_ref().map(|n| n.tone())),
        Some(geode_tile::notice::Tone::Warning)
    );
    assert_eq!(h.tree(&vcx), LINES.to_vec(), "sheet order again");
}

#[gpui::test]
fn sheet_order_verbs_refuse_under_a_sort_naming_sort_clear(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 3 C", "SPX Z26 1 C", "SPX Z26 2 C"]);
    h.command(&mut vcx, "sort strike").unwrap();
    let before = sheet_order(&h, &vcx);
    for verb in ["move_down", "move_up"] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(MOVE_SORTED), "{verb}");
    }
    press(&h, &mut vcx, "shift+j");
    assert_eq!(h.footer(&vcx).as_deref(), Some(MOVE_SORTED), "the key too");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(MOVE_SORTED), "a selection");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(h.footer(&vcx).as_deref(), Some(PACKAGE_SORTED));
    assert_eq!(h.command(&mut vcx, "package 2"), Err(PACKAGE_SORTED.into()));
    assert_eq!(sheet_order(&h, &vcx), before, "nothing moved");
    h.command(&mut vcx, "sort clear").unwrap();
    h.dispatch(&mut vcx, "move_down", None);
    assert_ne!(sheet_order(&h, &vcx), before, "sheet order moves again");
}

#[gpui::test]
fn an_insert_under_a_sort_lands_in_sheet_order_and_the_cursor_follows_it_to_where_it_sorts(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 3000 C", "SPX Z26 1000 C", "SPX Z26 2000 C"]);
    h.command(&mut vcx, "sort strike").unwrap();
    // Painted 1000, 2000, 3000: the cursor on 3000 (sheet row 0).
    h.motion(&mut vcx, "bottom", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 3000 C"));
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "SPX Z26 1500 C");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.entry_error(&vcx), None);
    assert_eq!(
        sheet_order(&h, &vcx),
        vec![
            "SPX Z26 3000 C",
            "SPX Z26 1500 C",
            "SPX Z26 1000 C",
            "SPX Z26 2000 C"
        ],
        "below the cursor line in the sheet"
    );
    assert_eq!(
        h.tree(&vcx),
        vec![
            "SPX Z26 1000 C",
            "SPX Z26 1500 C",
            "SPX Z26 2000 C",
            "SPX Z26 3000 C"
        ],
        "painted where it sorts"
    );
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("SPX Z26 1500 C"),
        "the cursor follows the new line"
    );
}

#[gpui::test]
fn a_price_refresh_re_ranks_a_measure_sort_and_the_cursor_stays_on_its_line(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 1 C", "SPX Z26 2 C", "SPX Z26 3 C"]);
    let first = |s: &str| match s {
        "SPX Z26 1 C" => 10.0,
        "SPX Z26 2 C" => 30.0,
        _ => 20.0,
    };
    answer_by(&h, &mut vcx, first);
    h.command(&mut vcx, "sort npv desc").unwrap();
    assert_eq!(
        h.tree(&vcx),
        vec!["SPX Z26 2 C", "SPX Z26 3 C", "SPX Z26 1 C"]
    );
    // The cursor on line 3, painted second.
    h.motion(&mut vcx, "top", None);
    h.motion(&mut vcx, "down", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 3 C"));

    // The same prices again: the order stands and nothing rebuilds.
    h.dispatch(&mut vcx, "price", None);
    let builds = crate::grid::builds();
    answer_by(&h, &mut vcx, first);
    assert_eq!(
        crate::grid::builds(),
        builds,
        "an unchanged order refills only"
    );

    // Line 3 rallies past both: an order that is not sheet order.
    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, |s| {
        if s == "SPX Z26 3 C" { 50.0 } else { first(s) }
    });
    assert_eq!(
        h.tree(&vcx),
        vec!["SPX Z26 3 C", "SPX Z26 2 C", "SPX Z26 1 C"],
        "the live price re-ranked the rows"
    );
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("SPX Z26 3 C"),
        "the cursor rode its line to the top"
    );
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(0));
    assert_eq!(
        h.cell(&vcx, 0, "npv"),
        "50.00",
        "the painted window follows"
    );
}

#[gpui::test]
fn find_and_yank_walk_the_painted_order(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 3 C", "SPX Z26 1 C", "SPX Z26 2 C"]);
    h.command(&mut vcx, "sort strike").unwrap();
    // Painted 1, 2, 3. From the top, `n` walks down the screen.
    h.motion(&mut vcx, "top", None);
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 1 C"));
    vcx.update(|window, cx| {
        h.content.find(
            geode_shell::module::FindEvent::Committed("spx".into()),
            window,
            cx,
        )
    });
    let mut seen = Vec::new();
    for _ in 0..3 {
        h.dispatch(&mut vcx, "find_next", None);
        seen.push(cursor_text(&h, &vcx).unwrap());
    }
    assert_eq!(seen, vec!["SPX Z26 2 C", "SPX Z26 3 C", "SPX Z26 1 C"]);

    h.motion(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "yank", None);
    let register = h.tile.read_with(&vcx, |t, _| {
        t.register.as_ref().map(|r| r.len()).unwrap_or_default()
    });
    assert_eq!(register, 3);
    let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(
        clip.as_deref(),
        Some("SPX Z26 1 C\nSPX Z26 2 C\nSPX Z26 3 C"),
        "the yank keeps the screen's order"
    );
}

#[gpui::test]
fn a_sort_ranks_groups_by_their_folded_value(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &["SPX Z26 1 C", "NDX Z26 1 C", "SPX Z26 2 C", "HSI Z26 1 C"],
    );
    answer_by(&h, &mut vcx, |s| match s {
        "SPX Z26 1 C" => 5.0,
        "SPX Z26 2 C" => 6.0,
        "NDX Z26 1 C" => 20.0,
        _ => 1.0,
    });
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.command(&mut vcx, "sort npv desc").unwrap();
    // Groups are closed: their painted labels in rank order.
    assert_eq!(
        h.tree(&vcx),
        vec!["NDX", "SPX", "HSI"],
        "NDX 20, SPX 5 + 6, HSI 1"
    );
    let spec = h.tile.read_with(&vcx, |t, _| t.sort);
    assert_eq!(
        spec,
        Some(SortSpec {
            column: "npv",
            order: SortOrder::Desc
        })
    );
}

fn selected_rows(h: &Harness, vcx: &VisualTestContext) -> Option<std::ops::Range<usize>> {
    h.tile
        .read_with(vcx, |t, _| t.resolved.as_ref().map(|r| r.rows.clone()))
}

/// The review's probe: lines priced 10/30/20 under `npv desc`, `V` over
/// the top two rows, then line 1 reprices to 25 — between them. Ranked
/// live, the range would widen over it and `d` would delete it. The order
/// holds while the selection lives, and the tick's order applies when it
/// ends.
#[gpui::test]
fn a_price_tick_under_a_selection_holds_the_order_until_the_selection_ends(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 1 C", "SPX Z26 2 C", "SPX Z26 3 C"]);
    let first = |s: &str| match s {
        "SPX Z26 1 C" => 10.0,
        "SPX Z26 2 C" => 30.0,
        _ => 20.0,
    };
    answer_by(&h, &mut vcx, first);
    h.command(&mut vcx, "sort npv desc").unwrap();
    let held = vec!["SPX Z26 2 C", "SPX Z26 3 C", "SPX Z26 1 C"];
    assert_eq!(h.tree(&vcx), held);
    h.motion(&mut vcx, "top", None);
    press(&h, &mut vcx, "shift+v");
    h.motion(&mut vcx, "down", None);
    assert_eq!(selected_rows(&h, &vcx), Some(0..2));

    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, |s| {
        if s == "SPX Z26 1 C" { 25.0 } else { first(s) }
    });
    assert_eq!(h.tree(&vcx), held, "the order holds under the selection");
    assert_eq!(
        selected_rows(&h, &vcx),
        Some(0..2),
        "the range did not widen"
    );
    assert_eq!(h.cell(&vcx, 2, "npv"), "25.00", "values refill in place");

    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(
        sheet_order(&h, &vcx),
        vec!["SPX Z26 1 C"],
        "d removed exactly the two selected lines"
    );

    // Again, ended by escape: the deferred order applies then.
    h.dispatch(&mut vcx, "undo", None);
    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, first);
    assert_eq!(h.tree(&vcx), held);
    h.motion(&mut vcx, "top", None);
    press(&h, &mut vcx, "shift+v");
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, |s| {
        if s == "SPX Z26 1 C" { 25.0 } else { first(s) }
    });
    assert_eq!(h.tree(&vcx), held);
    press(&h, &mut vcx, "escape");
    assert_eq!(
        h.tree(&vcx),
        vec!["SPX Z26 2 C", "SPX Z26 1 C", "SPX Z26 3 C"],
        "the tick's order applies once the selection ends"
    );
}

/// A sort change with a live selection ends it first and says so: a
/// header click, and `:sort`.
#[gpui::test]
fn a_sort_change_clears_a_live_selection_and_says_why(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &LINES);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "down", None);
    assert!(selected_rows(&h, &vcx).is_some());
    click_sort_icon(&h, &mut vcx, STRIKE + 1);
    assert_eq!(sort(&h, &vcx), Some(("strike", SortOrder::Desc)));
    assert_eq!(selected_rows(&h, &vcx), None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(SORT_CLEARED_SELECTION));

    press(&h, &mut vcx, "shift+v");
    h.motion(&mut vcx, "down", None);
    assert!(selected_rows(&h, &vcx).is_some());
    h.command(&mut vcx, "sort npv").unwrap();
    assert_eq!(selected_rows(&h, &vcx), None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(SORT_CLEARED_SELECTION));
}

/// `g p` over rows adjacent on screen but apart in the sheet names the
/// sort, as the other sheet-order refusals do.
#[gpui::test]
fn g_p_over_rows_apart_in_the_sheet_names_the_sort(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 3 C", "SPX Z26 1 C", "SPX Z26 2 C"]);
    h.command(&mut vcx, "sort strike").unwrap();
    // Painted 1, 2, 3: rows 0..2 are sheet rows 1 and 2 — contiguous.
    // Rows 1..3 are sheet rows 2 and 0 — apart.
    h.motion(&mut vcx, "top", None);
    h.motion(&mut vcx, "down", None);
    press(&h, &mut vcx, "shift+v");
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(GROUP_SORTED));
    assert_eq!(h.sheet_len(&vcx), 3, "nothing packaged");
}

/// Autosize fits the label as painted: `|x|` under an absolute sort.
#[gpui::test]
fn autosize_fits_an_absolute_sorts_label(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &LINES);
    let width = |h: &Harness, vcx: &mut VisualTestContext| {
        h.command(vcx, "autosize").unwrap();
        h.tile.read_with(vcx, |t, cx| {
            t.table.read(cx).delegate().fitted.get("npv").copied()
        })
    };
    let plain = width(&h, &mut vcx).expect("fitted");
    h.command(&mut vcx, "sort npv abs").unwrap();
    let abs = width(&h, &mut vcx).expect("fitted");
    assert!(abs > plain, "{abs} > {plain}: the suffix is measured");
}

/// Barrier lines in sheet order 6000, 5500, 5900: `:sort barrier desc`
/// paints 6000, 5900, 5500.
const BARRIERS: [&str; 3] = [
    "SPX Z26 5000 C UO 6000",
    "SPX Z26 5000 C UO 5500",
    "SPX Z26 5000 C UO 5900",
];

fn select_top_two_under_barrier_sort(h: &Harness, vcx: &mut VisualTestContext) {
    h.command(vcx, "view barrier").unwrap();
    h.command(vcx, "sort barrier desc").unwrap();
    assert_eq!(h.tree(vcx)[1], "SPX Z26 5000 C UO 5900", "fixture: sorted");
    h.motion(vcx, "top", None);
    press(h, vcx, "shift+v");
    h.motion(vcx, "down", None);
    assert_eq!(selected_rows(h, vcx), Some(0..2));
}

/// The review's probe: a view switch that drops the sort ends a live
/// selection, as any sort change does — else the rows return to sheet
/// order under it and its range covers 5500, which `d` would delete.
#[gpui::test]
fn a_view_switch_dropping_the_sort_ends_a_live_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BARRIERS);
    select_top_two_under_barrier_sort(&h, &mut vcx);
    h.command(&mut vcx, "view vanilla").unwrap();
    assert_eq!(sort(&h, &vcx), None);
    assert_eq!(selected_rows(&h, &vcx), None, "the selection ended");
    assert_eq!(h.footer(&vcx).as_deref(), Some(SORT_CLEARED_SELECTION));
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("sort on 'barrier' dropped: the column is no longer in this view")
    );
    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(h.sheet_len(&vcx), 2, "d d took the cursor line alone");
}

/// The same drop through a views reload that loses the column.
#[gpui::test]
fn a_views_reload_dropping_the_sort_ends_a_live_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BARRIERS);
    select_top_two_under_barrier_sort(&h, &mut vcx);
    let doc = geode_core::config::merge_docs(
        "views",
        &[geode_core::config::LayerDoc::builtin(
            "views",
            "[barrier]\ndataset = \"pricer\"\n[[barrier.columns]]\nname = \"qty\"\nkind = \"dimension\"\n[[barrier.columns]]\nname = \"npv\"\n",
        )
        .unwrap()],
    );
    let (views, diags) = Views::from_specs(&geode_core::view::ViewSpec::from_doc(&doc).0);
    assert!(diags.is_empty(), "{diags:?}");
    vcx.update(|_, cx| {
        h.factory.reload(
            views,
            TemplateSet::builtin(),
            NamedColours::default(),
            None,
            std::time::Duration::from_secs(60),
            None,
            cx,
        )
    });
    vcx.run_until_parked();
    assert_eq!(sort(&h, &vcx), None);
    assert_eq!(selected_rows(&h, &vcx), None, "the selection ended");
    assert_eq!(h.footer(&vcx).as_deref(), Some(SORT_CLEARED_SELECTION));
}

/// A verb that takes the selection and then refuses puts it back with
/// the order it held: a later tick must not re-rank under it.
#[gpui::test]
fn a_refused_selection_verb_keeps_the_held_order(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 1 C", "SPX Z26 2 C", "SPX Z26 3 C"]);
    let first = |s: &str| match s {
        "SPX Z26 1 C" => 10.0,
        "SPX Z26 2 C" => 30.0,
        _ => 20.0,
    };
    answer_by(&h, &mut vcx, first);
    h.command(&mut vcx, "sort npv desc").unwrap();
    let held = vec!["SPX Z26 2 C", "SPX Z26 3 C", "SPX Z26 1 C"];
    h.motion(&mut vcx, "top", None);
    press(&h, &mut vcx, "shift+v");
    h.motion(&mut vcx, "down", None);
    // A tick ranks line 1 between the selected two, held.
    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, |s| {
        if s == "SPX Z26 1 C" { 25.0 } else { first(s) }
    });
    assert_eq!(h.tree(&vcx), held);
    h.tile.update(&mut vcx, |t, _| t.refuse_next_edit = true);
    h.dispatch(&mut vcx, "delete", None);
    assert!(h.footer(&vcx).is_some(), "the delete refused");
    assert_eq!(h.sheet_len(&vcx), 3);
    assert_eq!(h.tree(&vcx), held, "the restored selection's order holds");
    assert_eq!(
        selected_rows(&h, &vcx),
        Some(0..2),
        "the range did not widen"
    );
    h.dispatch(&mut vcx, "price", None);
    answer_by(&h, &mut vcx, |s| {
        if s == "SPX Z26 1 C" { 26.0 } else { first(s) }
    });
    assert_eq!(h.tree(&vcx), held, "a later tick still holds");
    assert_eq!(selected_rows(&h, &vcx), Some(0..2));
}
