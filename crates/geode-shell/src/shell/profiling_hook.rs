//! Optional GPUI profiler actions, compiled with the `profiling` feature.
//! That feature enables `gpui/profiler`; the default build omits this
//! module and its actions. No Tracy backend is configured here.
//!
//! `perf::gpui_overlay` cycles GPUI's painted frame overlay. `perf::dump`
//! logs the shell's render-interval summary and GPUI's draw, dirty-to-present,
//! and present-interval histograms at `info` on `geode::shell`. Logging runs
//! on explicit action dispatch, outside the render path.

use gpui::{Context, Window};

use super::ShellView;
use crate::actions::ActionId;
use crate::perf::format_ms;

/// Handle the profiler-feature actions. Called from `ShellView::dispatch`'s
/// tail for any action id no always-compiled branch claimed. Returns
/// whether the id was recognised — `dispatch` falls through to the
/// focused occupant otherwise, exactly as it would with this feature off.
pub fn dispatch(
    view: &mut ShellView,
    action: &ActionId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    match action.0.as_str() {
        // Advance gpui's own painted frame overlay through its
        // Hidden → Minimal → Full cycle (it schedules its own redraw).
        "perf::gpui_overlay" => {
            window.cycle_debug_frame_overlay_mode();
            cx.notify();
            true
        }
        // Log both measurement layers: Geode's always-compiled
        // render-interval histogram and gpui's draw/present histograms.
        "perf::dump" => {
            let shell = &view.perf;
            tracing::info!(
                target: "geode::shell",
                "shell render intervals: n={} p50={} p95={} max={} idle-gaps={}",
                shell.count(),
                shell
                    .percentile_micros(50.0)
                    .map_or_else(|| "-".into(), format_ms),
                shell
                    .percentile_micros(95.0)
                    .map_or_else(|| "-".into(), format_ms),
                format_ms(shell.max_micros()),
                shell.discarded_idle(),
            );
            let snapshot = window.frame_duration_snapshot();
            // A loop over concrete references, not a named helper fn: the
            // histograms are hdrhistogram `Histogram<u64>`s (nanosecond
            // values), a type gpui does not re-export — method calls need
            // no type name, so this shape avoids adding hdrhistogram as a
            // direct dependency just to write a function signature.
            for (label, hist) in [
                ("gpui draw duration", &snapshot.draw_duration_histogram),
                ("gpui dirty->present", &snapshot.dirty_to_present_histogram),
                (
                    "gpui present interval (animating)",
                    &snapshot.present_interval_histogram,
                ),
            ] {
                tracing::info!(
                    target: "geode::shell",
                    "{label}: n={} p50={} p95={} max={}",
                    hist.len(),
                    format_ms(hist.value_at_quantile(0.50) / 1_000),
                    format_ms(hist.value_at_quantile(0.95) / 1_000),
                    format_ms(hist.max() / 1_000),
                );
            }
            true
        }
        _ => false,
    }
}
