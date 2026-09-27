//! The Perf section's body. A placeholder until the histogram, budgets, and
//! the overlay switch land; the model is already built by the page.

use gpui::prelude::*;
use gpui::{AnyElement, Context, WeakEntity, div};
use gpui_component::ActiveTheme as _;

use crate::model::PerfModel;
use crate::page::DiagnosticsPage;

pub(crate) fn render(
    _model: Option<&PerfModel>,
    _weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    div()
        .p_2()
        .text_xs()
        .text_color(muted)
        .child("perf: no samples yet")
        .into_any_element()
}
