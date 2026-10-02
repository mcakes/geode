//! The expiry strip: a column beside the chart, one row per expiry of the
//! strip, each with its color dot (filled when active), its date and a
//! mark cell per kind showing the kind's digit where that kind has the
//! expiry. The cursor row takes the selection background while the tile is
//! focused.
//!
//! [`prepare`] formats the dates and resolves the colors when the strip,
//! the active set or the palette change; paint clones prepared handles.

use chrono::{Datelike as _, NaiveDate};
use geode_chart::core::palette::Palette;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{Div, Entity, Hsla, MouseButton, MouseDownEvent, SharedString, Stateful, div, rems};
use gpui_component::{Theme, h_flex, v_flex};

use crate::core::model::{Kind, State, StripRow};
use crate::tile::VolsliceTile;

/// The strip column's width, in rems: a date and three mark cells.
pub(crate) const STRIP_WIDTH_REM: f32 = 7.5;
/// The color dot's diameter at the design rem.
const DOT: f32 = 8.0;
/// One strip row's height at the design rem.
const ROW_HEIGHT: f32 = 20.0;

const MARKS: [&str; 3] = ["1", "2", "3"];

/// One strip row, ready to paint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripPaint {
    pub expiry: NaiveDate,
    pub date: SharedString,
    /// The expiry's palette color, by strip position.
    pub color: Hsla,
    pub active: bool,
    /// Indexed by [`Kind::index`]: the kind's digit where it has the expiry.
    pub marks: [Option<&'static str>; 3],
}

pub(crate) fn prepare(strip: &[StripRow], state: &State, palette: &Palette) -> Vec<StripPaint> {
    strip
        .iter()
        .enumerate()
        .map(|(i, r)| StripPaint {
            expiry: r.expiry,
            date: r.expiry.to_string().into(),
            color: palette.colour(i),
            active: state.active.as_ref().is_some_and(|a| a.contains(&r.expiry)),
            marks: Kind::ALL.map(|k| r.has[k.index()].then_some(MARKS[k.index()])),
        })
        .collect()
}

/// A row's stable id: its expiry, as a day number.
fn row_id(expiry: NaiveDate) -> gpui::ElementId {
    gpui::ElementId::NamedInteger(
        SharedString::new_static("volslice-strip"),
        expiry.num_days_from_ce() as u64,
    )
}

pub(crate) fn render_strip(
    rows: &[StripPaint],
    cursor: usize,
    focused: bool,
    theme: &Theme,
    tile: &Entity<VolsliceTile>,
    tile_id: u64,
) -> Stateful<Div> {
    let mut column = v_flex()
        .id(gpui::ElementId::NamedInteger(
            SharedString::new_static("volslice-strip-column"),
            tile_id,
        ))
        .debug_selector(move || format!("volslice-strip-{tile_id}"))
        .w(rems(STRIP_WIDTH_REM))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .py_1()
        .border_l_1()
        .border_color(theme.border)
        .text_xs();
    for (i, row) in rows.iter().enumerate() {
        let expiry = row.expiry;
        let lit = focused && i == cursor;
        let dot = div()
            .size(scale::design(DOT))
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(row.color)
            .when(row.active, |d| d.bg(row.color));
        let mut el = h_flex()
            .id(row_id(expiry))
            .debug_selector(move || format!("volslice-strip-{tile_id}-{expiry}"))
            .h(scale::design(ROW_HEIGHT))
            .px_1()
            .gap_1()
            .items_center()
            // The list's active-row token: the strip is a list, and the
            // input-selection token is for selected text.
            .when(lit, |d| d.bg(theme.list_active))
            .text_color(if row.active {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .child(dot)
            .child(div().flex_1().child(row.date.clone()));
        for mark in row.marks {
            el = el.child(
                div()
                    .w(scale::design(DOT))
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .children(mark),
            );
        }
        // A press on the row: solo, or toggle with ctrl. The shell's own
        // tile handler runs after this one and focuses the tile; the tile
        // acts only when it was already focused, so the press that focuses
        // it does nothing else.
        el = el.on_mouse_down(MouseButton::Left, {
            let tile = tile.clone();
            move |event: &MouseDownEvent, _window, cx| {
                tile.update(cx, |t, cx| {
                    t.strip_pressed(expiry, event.modifiers.control, cx)
                });
            }
        });
        column = column.child(el);
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::strip;
    use crate::core::model::tests::{TODAY, d, fixture};

    #[test]
    fn rows_carry_their_date_color_activity_and_kind_marks() {
        let s = strip(&fixture(), d(TODAY));
        let mut st = State::default();
        st.reconcile(&s);
        let palette = Palette::from_theme(
            [
                gpui::red(),
                gpui::green(),
                gpui::blue(),
                gpui::yellow(),
                gpui::white(),
            ],
            gpui::black(),
            gpui::white(),
        );
        let rows = prepare(&s, &st, &palette);
        assert_eq!(rows[0].date.as_ref(), "2026-10-16");
        assert!(rows[0].active && !rows[1].active);
        assert_eq!(rows[0].marks, [Some("1"), Some("2"), None]);
        assert_eq!(rows[1].marks, [None, None, Some("3")]);
        assert_eq!(rows[1].color, palette.colour(1));
        assert_ne!(row_id(rows[0].expiry), row_id(rows[1].expiry));
    }
}
