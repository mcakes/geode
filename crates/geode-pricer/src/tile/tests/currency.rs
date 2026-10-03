//! A line's payout currency through its production routes: the entry
//! bar's default from reference data, a blank line's `needs currency`,
//! the refresh fill (never an undo step, never over a set currency), the
//! re-lookup an underlying edit makes on a blank line, a typed currency,
//! a load, a put, and no payout source.

use super::*;

/// A sheet named `book` whose lines carry the given currencies, and the
/// session record that restores it.
fn seeded_in(lines: &[(&str, Option<&str>)]) -> (MemorySheetStore, toml::Table) {
    let mut s = Sheet::new("book");
    let rows: Vec<RowSpec> = lines
        .iter()
        .map(|(l, c)| {
            let c = c.map(|c| Currency::parse(c).unwrap());
            match crate::core::shorthand::parse_builtin(l).unwrap() {
                RowSpec::Line(mut spec) => {
                    spec.currency = c;
                    RowSpec::Line(spec)
                }
                RowSpec::Package { template, mut legs } => {
                    for leg in &mut legs {
                        leg.currency = c;
                    }
                    RowSpec::Package { template, legs }
                }
            }
        })
        .collect();
    s.apply(Edit::Insert {
        place: Place::Root { at: 0 },
        rows,
    })
    .unwrap();
    let store = MemorySheetStore::default();
    assert!(store.save("book", to_rows(&s).unwrap()).is_ok());
    let mut t = toml::Table::new();
    t.insert("sheet".into(), "book".into());
    (store, t)
}

fn open_in(
    cx: &mut gpui::TestAppContext,
    lines: &[(&str, Option<&str>)],
) -> (Harness, VisualTestContext) {
    let (store, record) = seeded_in(lines);
    let (h, mut vcx) = open_full(cx, Some(record), store, test_settings());
    h.visible(&mut vcx, true);
    (h, vcx)
}

/// The currency each priced line was asked in, by line id, over every
/// request since the last drain.
fn asked(h: &Harness) -> Vec<(u64, Currency)> {
    h.prices()
        .iter()
        .flat_map(|p| p.lines.iter().map(|l| (l.id, l.request.currency)))
        .collect()
}

fn can_undo(h: &Harness, vcx: &VisualTestContext) -> bool {
    h.tile.read_with(vcx, |t, _| t.undo.can_undo())
}

fn line_id(h: &Harness, vcx: &VisualTestContext, row: usize) -> u64 {
    h.tile.read_with(vcx, |t, _| t.sheet.id(row).0)
}

/// Commit `text` in the cursor row's `column` through the cell editor.
fn edit_cell(h: &Harness, vcx: &mut VisualTestContext, column: &str, text: &str) {
    goto_column(h, vcx, column);
    h.dispatch(vcx, "edit", None);
    set_editor(h, vcx, text);
    h.dispatch(vcx, "commit", None);
    h.draw(vcx);
}

#[gpui::test]
fn a_new_spx_line_gets_usd_from_reference_data(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.entry_error(&vcx), None);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    let id = line_id(&h, &vcx, 0);
    assert_eq!(asked(&h), vec![(id, Currency::USD)]);
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.sheet.currency(0) == Some(Currency::USD)),
        "the currency is the line's own, saved with it"
    );
}

/// A package's legs each look up their own underlying.
#[gpui::test]
fn a_new_package_gives_each_leg_its_reference_currency(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.entry_error(&vcx), None);
    let legs = h.tile.read_with(&vcx, |t, _| {
        t.sheet
            .children(0)
            .map(|r| t.sheet.currency(r))
            .collect::<Vec<_>>()
    });
    assert_eq!(legs, vec![Some(Currency::USD); 2]);
    assert_eq!(asked(&h).len(), 2, "both legs are asked");
}

#[gpui::test]
fn an_unmapped_underlying_lands_blank_and_needs_currency(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "AAPL Z26 100 C");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.entry_error(&vcx), None);
    assert_eq!(h.sheet_len(&vcx), 1);
    assert_eq!(h.cell(&vcx, 0, "currency"), "");
    assert_eq!(h.cell(&vcx, 0, "status"), "needs currency");
    assert_eq!(
        asked(&h),
        vec![],
        "no request for a line without a currency"
    );
}

#[gpui::test]
fn a_reference_refresh_fills_only_blank_currencies(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(
        cx,
        &[("AAPL Z26 100 C", None), ("SPX Z26 5000 C", Some("EUR"))],
    );
    assert_eq!(h.cell(&vcx, 0, "currency"), "", "fixture: AAPL is unmapped");
    assert_eq!(h.cell(&vcx, 1, "currency"), "EUR");
    assert!(!dirty(&h, &vcx), "fixture: a load that filled nothing");
    assert!(!can_undo(&h, &vcx));
    // SPX answered (in USD, so it fails against EUR): not stale, so the
    // refill's batch can only carry what the fill touched.
    let opened = h.prices();
    assert_eq!(opened.len(), 1, "fixture: one batch at open");
    h.answer(&mut vcx, &opened[0], 1.0);

    publish_reference(&mut vcx, &[("AAPL", "USD"), ("SPX", "USD")]);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    assert_eq!(h.cell(&vcx, 1, "currency"), "EUR", "a set currency stays");
    let aapl = line_id(&h, &vcx, 0);
    assert_eq!(
        asked(&h),
        vec![(aapl, Currency::USD)],
        "only the filled line"
    );
    assert!(dirty(&h, &vcx), "the fill changed the sheet: it saves");
    assert!(!can_undo(&h, &vcx), "the fill is no undo step");
    h.dispatch(&mut vcx, "undo", None);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD", "undo does not unfill");
}

/// A republish that resolves nothing new wakes the tile but changes
/// nothing: no request, no save.
#[gpui::test]
fn a_reference_refresh_that_fills_nothing_changes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(cx, &[("AAPL Z26 100 C", None)]);
    let _ = h.prices();
    publish_reference(&mut vcx, &[("SPX", "USD")]);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "");
    assert_eq!(asked(&h), vec![]);
    assert!(!dirty(&h, &vcx));
}

/// The lookup is part of the trader's edit: `u` takes back the
/// underlying and the currency it brought together, so the line never
/// keeps a currency looked up for an underlying it no longer has; redo
/// replays both.
#[gpui::test]
fn editing_a_blank_lines_underlying_looks_its_currency_up(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(cx, &[("AAPL Z26 100 C", None)]);
    let _ = h.prices();
    edit_cell(&h, &mut vcx, "underlying_ref", "SPX");
    assert_eq!(h.cell(&vcx, 0, "underlying_ref"), "SPX");
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    let id = line_id(&h, &vcx, 0);
    assert_eq!(asked(&h), vec![(id, Currency::USD)]);
    h.dispatch(&mut vcx, "undo", None);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "underlying_ref"), "AAPL");
    assert_eq!(
        h.cell(&vcx, 0, "currency"),
        "",
        "undo unfills with the edit"
    );
    assert_eq!(h.cell(&vcx, 0, "status"), "needs currency");
    assert!(!can_undo(&h, &vcx), "one undo entry");
    h.dispatch(&mut vcx, "redo", None);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "underlying_ref"), "SPX");
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD", "redo replays both");
}

#[gpui::test]
fn editing_a_set_lines_underlying_keeps_its_currency(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(cx, &[("SPX Z26 5000 C", Some("EUR"))]);
    let _ = h.prices();
    edit_cell(&h, &mut vcx, "underlying_ref", "SX5E");
    assert_eq!(h.cell(&vcx, 0, "underlying_ref"), "SX5E");
    assert_eq!(h.cell(&vcx, 0, "currency"), "EUR");
    let id = line_id(&h, &vcx, 0);
    assert_eq!(asked(&h), vec![(id, Currency::parse("EUR").unwrap())]);
}

/// A package's underlying edit moves every leg; each blank leg looks up
/// its own new underlying.
#[gpui::test]
fn editing_a_blank_packages_underlying_looks_each_leg_up(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(cx, &[("AAPL Z26 100/110 CS", None)]);
    let _ = h.prices();
    edit_cell(&h, &mut vcx, "underlying_ref", "SPX");
    let legs = h.tile.read_with(&vcx, |t, _| {
        t.sheet
            .children(0)
            .map(|r| t.sheet.currency(r))
            .collect::<Vec<_>>()
    });
    assert_eq!(legs, vec![Some(Currency::USD); 2]);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    assert_eq!(asked(&h).len(), 2);
    h.dispatch(&mut vcx, "undo", None);
    h.draw(&mut vcx);
    let legs = h.tile.read_with(&vcx, |t, _| {
        t.sheet
            .children(0)
            .map(|r| {
                (
                    t.sheet.instrument(r).unwrap().underlying().to_string(),
                    t.sheet.currency(r),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        legs,
        vec![("AAPL".to_string(), None); 2],
        "undo unfills every leg"
    );
}

#[gpui::test]
fn typing_eur_in_the_currency_cell_reprices(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
    let id = line_id(&h, &vcx, 0);
    assert_eq!(
        asked(&h),
        vec![(id, Currency::USD)],
        "fixture: priced in USD"
    );
    goto_column(&h, &mut vcx, "currency");
    h.dispatch(&mut vcx, "edit", None);
    select_all_and_type(&h, &mut vcx, "eur");
    h.dispatch(&mut vcx, "commit", None);
    h.draw(&mut vcx);
    assert_eq!(h.mode(&mut vcx), "normal");
    assert_eq!(h.cell(&vcx, 0, "currency"), "EUR");
    assert_eq!(asked(&h), vec![(id, Currency::parse("EUR").unwrap())]);

    h.dispatch(&mut vcx, "edit", None);
    select_all_and_type(&h, &mut vcx, "EU");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        h.mode(&mut vcx),
        "insert",
        "a refusal keeps the editor open"
    );
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("a currency is three letters, e.g. USD")
    );
    assert_eq!(h.cell(&vcx, 0, "currency"), "EUR");
}

/// Both load routes: the synchronous read at open, and a pending one
/// answered later. The fill marks the sheet changed, so it saves.
#[gpui::test]
fn a_loaded_sheet_fills_its_blank_currencies(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_in(cx, &[("SPX Z26 5000 C", None), ("AAPL Z26 100 C", None)]);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    assert_eq!(h.cell(&vcx, 1, "currency"), "");
    assert!(dirty(&h, &vcx), "the filled sheet saves");
    assert!(!can_undo(&h, &vcx));
    let spx = line_id(&h, &vcx, 0);
    assert_eq!(asked(&h), vec![(spx, Currency::USD)]);
}

#[gpui::test]
fn a_pending_load_fills_its_blank_currencies_once_it_lands(cx: &mut gpui::TestAppContext) {
    let (store, record) = seeded_in(&[("SPX Z26 5000 C", None)]);
    let rows = store.get("book").unwrap();
    store.set_pending(true);
    let (h, mut vcx) = open_full(cx, Some(record), store, test_settings());
    h.visible(&mut vcx, true);
    assert!(!dirty(&h, &vcx), "the empty fallback never saves");
    h.tile
        .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
    assert!(dirty(&h, &vcx), "the filled sheet saves");
    assert!(!can_undo(&h, &vcx));
    let id = line_id(&h, &vcx, 0);
    assert_eq!(asked(&h), vec![(id, Currency::USD)]);
}

/// A put keeps the yanked line's currency; a blank yanked line looks
/// its underlying up as a typed one would.
#[gpui::test]
fn a_put_keeps_a_set_currency_and_looks_a_blank_one_up(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(
        cx,
        &[("AAPL Z26 100 C", None), ("SPX Z26 5000 C", Some("EUR"))],
    );
    // The blank AAPL line in the register, then its own row moved off
    // AAPL so the refresh below has nothing on the sheet to fill.
    h.dispatch(&mut vcx, "yank_row", None);
    edit_cell(&h, &mut vcx, "underlying_ref", "NDX");
    publish_reference(&mut vcx, &[("AAPL", "JPY")]);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD", "fixture: NDX's");
    h.dispatch(&mut vcx, "put_below", None);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 1, "underlying_ref"), "AAPL");
    assert_eq!(h.cell(&vcx, 1, "currency"), "JPY");

    h.motion(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "yank_row", None);
    h.dispatch(&mut vcx, "put_below", None);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 3, "underlying_ref"), "SPX");
    assert_eq!(h.cell(&vcx, 3, "currency"), "EUR", "the yanked currency");
}

/// A reload that names a payout source fills what the old one left.
#[gpui::test]
fn a_reload_naming_a_payout_source_fills_blank_lines(cx: &mut gpui::TestAppContext) {
    let (store, record) = seeded_in(&[("SPX Z26 5000 C", None)]);
    let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
    h.visible(&mut vcx, true);
    assert_eq!(h.cell(&vcx, 0, "currency"), "", "fixture: no payout source");
    let (views, settings) = (h.factory.views_for_tests(), h.factory.settings());
    vcx.update(|_, cx| {
        h.factory.reload(
            views,
            TemplateSet::builtin(),
            NamedColours::default(),
            settings.refresh,
            settings.stale_after,
            test_settings().payout,
            cx,
        )
    });
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "USD");
}

/// A reload that leaves the payout source as it was (templates, views,
/// colors, refresh) fills nothing: a line the trader cleared stays blank.
#[gpui::test]
fn a_reload_keeping_the_payout_source_does_not_refill(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
    edit_cell(&h, &mut vcx, "currency", "");
    assert_eq!(h.cell(&vcx, 0, "currency"), "", "fixture: cleared");
    let (views, settings) = (h.factory.views_for_tests(), h.factory.settings());
    vcx.update(|_, cx| {
        h.factory.reload(
            views,
            TemplateSet::builtin(),
            NamedColours::default(),
            settings.refresh,
            settings.stale_after,
            settings.payout.clone(),
            cx,
        )
    });
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "");
}

/// A reference cell is read as the currency cell reads typed text:
/// trimmed and upper-cased.
#[gpui::test]
fn a_reference_cell_in_lower_case_or_padded_still_fills(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_in(cx, &[("AAPL Z26 100 C", None), ("NDX Z26 3000 C", None)]);
    assert_eq!(h.cell(&vcx, 1, "currency"), "USD", "fixture: NDX mapped");
    publish_reference(&mut vcx, &[("AAPL", " eur ")]);
    h.draw(&mut vcx);
    assert_eq!(h.cell(&vcx, 0, "currency"), "EUR");
}

#[gpui::test]
fn without_a_payout_source_new_lines_stay_blank(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_full(
        cx,
        None,
        MemorySheetStore::default(),
        PricerSettings::default(),
    );
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add_below", None);
    typed(&h, &mut vcx, "SPX Z26 5000 C");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.entry_error(&vcx), None);
    assert_eq!(h.cell(&vcx, 0, "currency"), "");
    assert_eq!(h.cell(&vcx, 0, "status"), "needs currency");
    assert_eq!(asked(&h), vec![]);
}
