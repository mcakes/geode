//! Profiler support (spec §7.4: "profiler support from day one"), compiled
//! only with geode-shell's `profiling` feature — which is nothing but
//! gpui's own `profiler` feature re-exported (`profiling = ["gpui/profiler"]`).
//!
//! **Findings at the pinned gpui rev (zed e3adf43), recorded decision:**
//! gpui already ships real profiler infrastructure behind its `profiler`
//! cargo feature — hdrhistogram-backed per-window frame-duration and
//! input-latency histograms (`Window::frame_duration_snapshot` /
//! `input_latency_snapshot`), a built-in painted debug frame overlay
//! (`Window::cycle_debug_frame_overlay_mode` — draws directly into the
//! scene, bypassing layout/invalidation so it can't feed back into what it
//! measures), plus hang-detection and task-timing journals. Its only extra
//! dependency is `hdrhistogram`, pulled in by gpui itself. It does NOT use
//! a Tracy client: gpui instruments via the backend-less `profiling` facade
//! crate (`profiling::function` / `finish_frame!`), and selecting a Tracy
//! backend would mean this workspace adding `tracy-client` — a new
//! dependency deliberately not taken. So Geode's profiler hook is exactly
//! gpui's own feature, surfaced through two palette actions here; the
//! default build is completely unchanged (verified: this module and the
//! actions it serves are absent without the feature).
//!
//! `perf::dump` logs a few summary lines at `info` (target `geode::shell`)
//! from inside an action dispatch — an explicit user request, not the
//! render path, so it doesn't bend the no-I/O-in-render rule (same class
//! as the config-warning `tracing` events elsewhere in this crate,
//! Phase 4b Task 2).

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
        // Dump both measurement layers to stderr: Geode's always-compiled
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
