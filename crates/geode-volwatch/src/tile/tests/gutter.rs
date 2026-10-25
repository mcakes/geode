//! The line-number gutter beside the Underlying cells: `[ui] line_numbers`
//! reaches the tile through the shell's `UiSettings` global, at
//! construction and on a change.

use geode_shell::linenumbers::{LineNumbers, UiSettings, gutter_px};
use geode_shell::module::FindEvent;

use super::harness::*;

// Selectors spell the tile id `7` ([`TILE`]) literally: `debug_bounds`
// takes a static string.

fn set(h: &Harness, vcx: &mut gpui::VisualTestContext, mode: LineNumbers) {
    vcx.update(|_, cx| cx.set_global(UiSettings { line_numbers: mode }));
    h.draw(vcx);
}

fn bounds(vcx: &mut gpui::VisualTestContext, sel: &'static str) -> gpui::Bounds<gpui::Pixels> {
    vcx.debug_bounds(sel)
        .unwrap_or_else(|| panic!("{sel} painted"))
}

/// The gutter text on every shown row, as the delegate paints it.
fn texts(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, cx| {
        let d = t.table.read(cx).delegate();
        (0..t.grid.visible().len())
            .map(|r| d.gutter_text(r).map(|s| s.to_string()).unwrap_or_default())
            .collect()
    })
}

/// Off paints no gutter. `on` paints one beside each Underlying cell,
/// widening the column by exactly the gutter and pushing the label right
/// by it. `rel` re-numbers on a cursor move, the cursor row keeping its
/// absolute number, which is the row `N shift+g` lands on. Off gives
/// the width back.
#[gpui::test]
fn the_line_numbers_global_paints_a_gutter_beside_the_underlying_cell(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("7-DAX-gutter").is_none(),
        "no gutter while the setting is off (no global set)"
    );
    let width_off = h.column_width(&vcx, "underlying");
    let cell_off = bounds(&mut vcx, "volwatch-td-7-DAX-underlying");
    let label_off = bounds(&mut vcx, "7-DAX-underlying-text");

    set(&h, &mut vcx, LineNumbers::On);
    let px = gutter_px(LineNumbers::On, 3);
    assert!(px > 0.0, "sanity: a live gutter has width");
    let gutter = bounds(&mut vcx, "7-DAX-gutter");
    assert!(
        (gutter.size.width - gpui::px(px)).abs() < gpui::px(0.5),
        "the gutter is the shell's width: {:?} vs {px}",
        gutter.size.width
    );
    assert!(
        (h.column_width(&vcx, "underlying") - (width_off + px)).abs() < 0.5,
        "the Underlying column's width includes the gutter"
    );
    let cell_on = bounds(&mut vcx, "volwatch-td-7-DAX-underlying");
    assert!(
        ((cell_on.size.width - cell_off.size.width) - gpui::px(px)).abs() < gpui::px(0.5),
        "the painted cell widened by the gutter: {:?} from {:?}; the tile must \
         refresh the table, which caches the column widths, or the name clips",
        cell_on.size.width,
        cell_off.size.width
    );
    let label_on = bounds(&mut vcx, "7-DAX-underlying-text");
    assert!(
        ((label_on.left() - label_off.left()) - gpui::px(px)).abs() < gpui::px(0.5),
        "the label moved right by the gutter"
    );
    assert!(
        gutter.right() <= label_on.left(),
        "the gutter sits before the label: {gutter:?} then {label_on:?}"
    );
    assert_eq!(texts(&h, &vcx), ["1", "2", "3"]);

    h.press(&mut vcx, "j");
    set(&h, &mut vcx, LineNumbers::Relative);
    assert_eq!(
        texts(&h, &vcx),
        ["1", "2", "1"],
        "cursor on row 2: its own number, then distances"
    );
    h.press(&mut vcx, "3 shift-g");
    assert_eq!(h.cursor(&vcx), Some("NEW".into()), "3 shift+g is row 3");
    assert_eq!(texts(&h, &vcx), ["2", "1", "3"], "a move re-numbers");
    set(&h, &mut vcx, LineNumbers::On);
    assert_eq!(texts(&h, &vcx), ["1", "2", "3"]);

    set(&h, &mut vcx, LineNumbers::Off);
    assert!(
        vcx.debug_bounds("7-DAX-gutter").is_none(),
        "off again on the next draw"
    );
    assert!(
        (h.column_width(&vcx, "underlying") - width_off).abs() < 0.5,
        "the column gave the width back"
    );
    let cell = bounds(&mut vcx, "volwatch-td-7-DAX-underlying");
    assert!(
        (cell.size.width - cell_off.size.width).abs() < gpui::px(0.5),
        "and the painted cell shrank back: {:?} vs {:?}",
        cell.size.width,
        cell_off.size.width
    );
    let label = bounds(&mut vcx, "7-DAX-underlying-text");
    assert!((label.left() - label_off.left()).abs() < gpui::px(0.5));
}

/// A tile opened with the setting already on paints the gutter from its
/// first frame. Group rows are numbered rows; the gutter sits before the
/// indent and the chevron, in one lane whatever the depth; a fold and a
/// `/` narrowing renumber the rows left, and a hidden member has none.
#[gpui::test]
fn the_gutter_numbers_group_rows_and_renumbers_on_a_fold_and_a_narrowing(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        cx.set_global(UiSettings {
            line_numbers: LineNumbers::On,
        })
    });
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("7-DAX-gutter").is_some(),
        "the setting is read at construction"
    );
    h.set_dims(&mut vcx, europe_dims());
    h.set_frame_chain(&mut vcx, &["region"]);
    h.draw(&mut vcx);
    assert_eq!(
        h.shown(&vcx),
        ["group:Core", "DAX", "SX5E", "group:\u{2014}", "NEW"]
    );
    assert_eq!(texts(&h, &vcx), ["1", "2", "3", "4", "5"]);
    let group = bounds(&mut vcx, "7-group:Core-gutter");
    let chevron = bounds(&mut vcx, "volwatch-fold-group:Core");
    assert!(
        group.right() <= chevron.left(),
        "the gutter sits before the chevron: {group:?} then {chevron:?}"
    );
    let member = bounds(&mut vcx, "7-DAX-gutter");
    assert!(
        (member.left() - group.left()).abs() < gpui::px(0.5),
        "a member's number sits in the group's lane; the indent starts after it"
    );
    let label = bounds(&mut vcx, "7-DAX-underlying-text");
    assert!(member.right() <= label.left());

    h.press(&mut vcx, "space");
    assert_eq!(h.shown(&vcx), ["group:Core", "group:\u{2014}", "NEW"]);
    h.draw(&mut vcx);
    assert_eq!(texts(&h, &vcx), ["1", "2", "3"], "a fold renumbers");
    assert!(
        vcx.debug_bounds("7-DAX-gutter").is_none(),
        "a hidden member has no number"
    );
    assert!(
        vcx.debug_bounds("volwatch-fold-group:Core").is_some(),
        "the chevron still paints on its row"
    );

    h.find(&mut vcx, FindEvent::Changed("NE".into()));
    assert_eq!(h.shown(&vcx), ["group:\u{2014}", "NEW"]);
    h.draw(&mut vcx);
    assert_eq!(texts(&h, &vcx), ["1", "2"], "a narrowing renumbers");
}
