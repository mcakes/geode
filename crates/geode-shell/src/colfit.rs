//! Fitting a table column to its content: the measure behind `:autosize`
//! in the table modules and the shell's `tile::autosize_columns` action.
//!
//! The table modules paint their cells in JetBrains Mono inside an
//! `XSmall` gpui-component `DataTable`, whose cells set `text_sm`. A
//! monospace glyph advances a fixed fraction of the font size, so a
//! column's width is its longest text's char count times that advance,
//! plus the cell's fixed chrome. Counting chars instead of shaping text
//! keeps the measure a pure function that the UI thread can afford over
//! every prepared cell at invocation. It is never run in render.
//!
//! Widths are pixels at the rem the window had when the fit ran. The
//! configured widths the fit replaces are pixels too, and
//! `TableDelegate::column` has no `Window` to rescale a rem-valued width
//! from, so a later font-size change leaves fitted columns sized for the
//! old scale, exactly like configured ones. Running the fit again refits.

use std::collections::BTreeMap;

use gpui::Pixels;
use gpui_component::Size;

/// JetBrains Mono (`fonts::MONO`) advances every glyph 600/1000 em.
pub const MONO_ADVANCE_EM: f32 = 0.6;
/// gpui-component's `XSmall` table cell paints at `text_sm`
/// (`StyleSized::table_cell_size`).
pub const TABLE_TEXT_REM: f32 = 0.875;
/// The cursor cell's `border_1`, both sides. It sits inside the cell box,
/// so a fitted column that ignored it would clip the cursor cell's text.
pub const CURSOR_BORDER_PX: f32 = 2.0;
/// The narrowest fitted column: about three characters, so an empty or
/// one-digit column stays wide enough to see, click, and read its header.
pub const MIN_REM: f32 = 2.5;
/// The widest fitted column: one runaway cell (a long failure reason, a
/// long label) would otherwise push every other column off-screen. Text
/// past this width ellipsizes or clips as it does at a configured width.
pub const MAX_REM: f32 = 40.0;

/// The refusal a tile without a table answers `tile::autosize_columns`
/// with, and the one the shell shows when no tile is focused.
pub const NO_TABLE: &str = "this tile has no table";

/// The refusal a table module answers a fit with while it has no rows to
/// measure (nothing delivered or loaded yet, or an empty result). The fitted
/// widths already held stay as they are; `reset` still works.
pub const NOTHING_TO_FIT: &str = "nothing loaded to fit";

/// The session-record key under which a table module stores its fitted
/// widths ([`widths_to_toml`], [`widths_from_record`]).
pub const SESSION_KEY: &str = "column_widths";

/// Fitted widths in pixels, keyed by a column's stable identity (its name
/// or key, never its position). Ordered, so a session record is stable.
pub type FittedWidths = BTreeMap<String, f32>;

/// What one fit measures with, resolved once per invocation from the
/// window's rem size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitMetrics {
    /// The window's rem, in pixels.
    pub rem_px: f32,
    /// One monospace glyph's advance, in pixels.
    pub advance_px: f32,
    /// Everything around the text: the cell padding on both sides, any
    /// padding a module's own cell adds, and the cursor border.
    pub padding_px: f32,
    pub min_px: f32,
    pub max_px: f32,
}

impl FitMetrics {
    /// An `XSmall` `DataTable` painting the mono font at `rem`: the
    /// component's own cell padding (`Size::XSmall.table_cell_padding`,
    /// read from the registry rather than restated) plus the cursor border.
    pub fn xsmall_mono(rem: Pixels) -> FitMetrics {
        let rem_px = f32::from(rem);
        let pad = Size::XSmall.table_cell_padding();
        FitMetrics {
            rem_px,
            advance_px: rem_px * TABLE_TEXT_REM * MONO_ADVANCE_EM,
            padding_px: f32::from(pad.left) + f32::from(pad.right) + CURSOR_BORDER_PX,
            min_px: rem_px * MIN_REM,
            max_px: rem_px * MAX_REM,
        }
    }

    /// Add horizontal padding a module's own cell element paints inside
    /// the component's cell (the blotter's `px_1`, for instance).
    pub fn with_extra_padding(mut self, px: f32) -> FitMetrics {
        self.padding_px += px;
        self
    }

    /// The painted width of `text` in the mono font, without padding.
    pub fn text_px(&self, text: &str) -> f32 {
        text.chars().count() as f32 * self.advance_px
    }

    /// The column width holding the widest of `content` (content widths in
    /// pixels, before padding): the maximum plus padding, rounded up to a
    /// whole pixel and clamped to `[min_px, max_px]`. No content at all
    /// gives the minimum.
    pub fn fit(&self, content: impl IntoIterator<Item = f32>) -> f32 {
        let widest = content.into_iter().fold(0.0_f32, f32::max);
        (widest + self.padding_px)
            .ceil()
            .clamp(self.min_px, self.max_px)
    }

    /// [`Self::fit`] over a header label and cell texts. A column with no
    /// cells fits its header.
    pub fn fit_text<'a>(&self, header: &str, cells: impl IntoIterator<Item = &'a str>) -> f32 {
        self.fit(
            std::iter::once(self.text_px(header)).chain(cells.into_iter().map(|c| self.text_px(c))),
        )
    }
}

/// The session value for `widths`, or `None` when there is nothing fitted
/// (the record then carries no key at all).
pub fn widths_to_toml(widths: &FittedWidths) -> Option<toml::Value> {
    if widths.is_empty() {
        return None;
    }
    Some(toml::Value::Table(
        widths
            .iter()
            .map(|(k, w)| (k.clone(), toml::Value::Float(f64::from(*w))))
            .collect(),
    ))
}

/// The narrowest width a restored record may carry: [`MIN_REM`] at the
/// smallest font scale. A fit never produces less at any scale, so a smaller
/// stored value was edited by hand or corrupted.
pub const RESTORED_MIN_PX: f32 = MIN_REM * 10.0;
/// The widest width a restored record may carry: [`MAX_REM`] at the largest
/// font scale, for the same reason. The scales are `FontSize`'s 10 and
/// 14 px rems; a test pins the two against it.
pub const RESTORED_MAX_PX: f32 = MAX_REM * 14.0;

/// Read [`SESSION_KEY`] from a restored record. Lenient like every session
/// read: a missing key, a value that is not a table, or an entry that is
/// not a positive finite number is dropped, never a failure. A kept entry is
/// clamped to [`RESTORED_MIN_PX`]..=[`RESTORED_MAX_PX`], the range a fit can
/// produce at any font scale, so a hand-edited `3e38` cannot build a column
/// wider than any layout.
pub fn widths_from_record(record: Option<&toml::Table>) -> FittedWidths {
    record
        .and_then(|t| t.get(SESSION_KEY))
        .and_then(|v| v.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| {
                    let w = v.as_float().or_else(|| v.as_integer().map(|i| i as f64))? as f32;
                    (w.is_finite() && w > 0.0)
                        .then(|| (k.clone(), w.clamp(RESTORED_MIN_PX, RESTORED_MAX_PX)))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    fn metrics() -> FitMetrics {
        FitMetrics::xsmall_mono(px(12.0))
    }

    #[test]
    fn the_metrics_follow_the_rem_and_the_components_padding() {
        let m = metrics();
        assert!((m.advance_px - 12.0 * 0.875 * 0.6).abs() < 1e-4);
        // XSmall: 4px either side, plus the 2px cursor border.
        assert_eq!(m.padding_px, 10.0);
        assert_eq!(m.min_px, 30.0);
        assert_eq!(m.max_px, 480.0);
        let large = FitMetrics::xsmall_mono(px(14.0));
        assert!(large.advance_px > m.advance_px, "a larger rem is wider");
    }

    #[test]
    fn the_widest_cell_decides_the_width() {
        let m = metrics();
        let w = m.fit_text("px", ["1.00", "-1,234,567.89", "12.5"]);
        let expected = (13.0 * m.advance_px + m.padding_px).ceil();
        assert_eq!(w, expected, "the longest cell, not the first or last");
        let reordered = m.fit_text("px", ["-1,234,567.89", "1.00", "12.5"]);
        assert_eq!(w, reordered);
    }

    #[test]
    fn a_long_header_dominates_short_cells() {
        let m = metrics();
        let header = "daily_trading_pnl";
        let w = m.fit_text(header, ["1", "22"]);
        assert_eq!(w, (17.0 * m.advance_px + m.padding_px).ceil());
    }

    #[test]
    fn an_empty_column_fits_its_header() {
        let m = metrics();
        assert_eq!(
            m.fit_text("underlying", std::iter::empty()),
            (10.0 * m.advance_px + m.padding_px).ceil()
        );
    }

    #[test]
    fn chars_are_counted_not_bytes() {
        let m = metrics();
        assert_eq!(m.text_px("Δ⋈"), 2.0 * m.advance_px);
    }

    #[test]
    fn widths_clamp_at_both_ends() {
        let m = metrics();
        assert_eq!(m.fit_text("", [""]), m.min_px, "nothing to show");
        assert_eq!(m.fit_text("x", ["1"]), m.min_px, "one char is below min");
        let long = "x".repeat(500);
        assert_eq!(m.fit_text("h", [long.as_str()]), m.max_px);
        assert_eq!(m.fit(std::iter::empty()), m.min_px);
    }

    #[test]
    fn extra_padding_adds_to_the_fit() {
        let m = metrics().with_extra_padding(6.0);
        assert_eq!(m.padding_px, 16.0);
    }

    #[test]
    fn widths_round_trip_and_garbage_reads_empty() {
        let mut w = FittedWidths::new();
        w.insert("delta01".into(), 84.0);
        w.insert(String::new(), 131.5);
        let mut record = toml::Table::new();
        record.insert(SESSION_KEY.into(), widths_to_toml(&w).unwrap());
        // Through text, as session.toml stores it.
        let text = toml::to_string(&record).unwrap();
        let back: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(widths_from_record(Some(&back)), w);

        assert!(widths_to_toml(&FittedWidths::new()).is_none());
        assert!(widths_from_record(None).is_empty());
        let garbage: toml::Table = toml::from_str("column_widths = \"wide\"\n").unwrap();
        assert!(widths_from_record(Some(&garbage)).is_empty());
        let mixed: toml::Table =
            toml::from_str("[column_widths]\na = 90\nb = \"x\"\nc = -3.0\nd = nan\n").unwrap();
        let read = widths_from_record(Some(&mixed));
        assert_eq!(read.len(), 1);
        assert_eq!(read["a"], 90.0);
    }

    #[test]
    fn a_restored_width_is_clamped_to_what_a_fit_can_produce() {
        let t: toml::Table = toml::from_str(
            "[column_widths]
huge = 3e38
tiny = 0.001
ok = 120.0
",
        )
        .unwrap();
        let read = widths_from_record(Some(&t));
        assert_eq!(read["huge"], RESTORED_MAX_PX);
        assert_eq!(read["tiny"], RESTORED_MIN_PX);
        assert_eq!(read["ok"], 120.0);
        use crate::fontsize::FontSize;
        assert_eq!(RESTORED_MIN_PX, MIN_REM * FontSize::Small.rem_px());
        assert_eq!(RESTORED_MAX_PX, MAX_REM * FontSize::Large.rem_px());
    }
}
