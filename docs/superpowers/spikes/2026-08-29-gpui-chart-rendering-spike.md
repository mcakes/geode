# Spike: can gpui's built-in drawing carry Geode's charting? (2026-08-29)

**Question.** For the future charting layer (high-performance derivative risk
visualization; `gpuchart/`, our earlier TypeScript/WebGPU plotting library, is
the architectural reference), can gpui's built-in drawing path — lyon-tessellated
`paint_path` strokes and `paint_quad` — hit Geode's <8ms pure-UI budget at
realistic data volumes, or must the framework be designed around custom GPU
pipelines?

**Verdict: gpui's primitives are sufficient, with one routing rule** — vector
geometry through `paint_path`, dense fields through `paint_image`. No custom
GPU pipeline, no gpui fork.

## Method

Throwaway gpui example (deleted after the spike): a `canvas` element animated a
phase offset every frame so every frame paid full data→screen mapping +
tessellation/image rebuild — the worst case, equivalent to continuous pan/zoom.
Measured paint-closure time (CPU cost of building/tessellating/pushing scene
primitives) and frame-to-frame deltas, p50/p95 over ~120 frames per stage.
Release build, macOS, gpui pinned at zed rev `e3adf43`.

## Results (paint p50)

| Workload | Route | Paint p50 |
|---|---|---|
| 5 polylines, 10k pts total | `paint_path` stroke | 1.2ms |
| 5 polylines, 100k–500k pts | `paint_path` | ~10–11ms |
| 5 polylines, 1M pts | `paint_path` | 12–16ms |
| Heatmap 10k cells | `paint_quad` per cell | 42ms |
| Heatmap 40k cells | `paint_quad` per cell | 551ms |
| Heatmap 100k cells | `paint_quad` per cell | 2936ms |
| Heatmap 100k cells | CPU pixels → `paint_image` | 0.3ms |
| Heatmap 1M cells | CPU pixels → `paint_image` | 2.2ms |
| Heatmap 4M cells | CPU pixels → `paint_image` | 6.7ms |
| Mixed: 100k-pt lines + 1M-cell heatmap | path + image | 11.6ms |

Frame deltas held a solid 60fps (16.6ms) in the interactive run up to and
including the 1M-point line stage.

## Findings that should shape the charting design

1. **Vector geometry (lines, bands, whiskers, smiles) → `paint_path`.** Huge
   headroom: 1M raw points holds 60fps, and real charts will decimate long
   before that. Caveat: the sublinear scaling above comes from lyon collapsing
   sub-pixel segments, which is *not* min-max decimation and can drop spikes
   between pixels — the data→geometry stage must do proper min-max decimation
   itself (needed for visual correctness anyway).
2. **Dense fields (heatmaps, vol surfaces, density overlays) → `paint_image`.**
   One pixel per cell, colormapped on CPU each frame, uploaded via the sprite
   atlas (fresh `RenderImage` per frame; `drop_image` the previous frame's
   image one frame later so the GPU is done with it), stretched to the plot
   rect by `paint_image`. A 1M-cell surface rebuilt every frame costs 2.2ms.
3. **Per-cell `paint_quad` is disqualified past ~5k cells** — cost blows up
   superlinearly in the scene/batching layer (10k = 42ms, 100k = 2.9s).
4. **An embedded wgpu renderer (gpuchart-style) is neither cleanly possible
   nor needed.** At the pinned rev, gpui's scene primitive set is closed
   (shadow/quad/path/underline/sprites, plus a macOS-only `CVPixelBuffer`
   surface) — no cross-platform texture hook, no custom shaders. The escape
   hatch, if primitives are ever outgrown: offscreen wgpu render → CPU
   readback → `paint_image`, priced by the image stages above at ~2–7ms for
   full-window fields. Viable without forking gpui.
5. **Continuous redraw must be driven by `window.request_animation_frame()`
   called from `render`** — it both schedules the next frame and notifies the
   view. Chaining `on_next_frame` + `cx.notify()` by hand stalled (callbacks
   coalesce into the current frame; the loop only advanced on incidental
   window events). Also: macOS delivers no frames to a locked/occluded
   session, and throttles unfocused windows to ~24fps — measure paint time,
   not frame deltas, in unattended runs.

## What to carry over from gpuchart

The architecture, not the code: framework-agnostic core (scales, series model,
hit-testing/hover bus, dirty scheduling, linked chart groups with shared
axes/crosshairs) as pure, window-free logic — matching Geode's testing
strategy — with a thin gpui element layer where its React layer sat. Its
per-series pipelines map onto the two routes: line/band/whisker/bubble → path,
heatmap/surface → image.

## Sequencing

Doesn't displace Phase 2 — charts consume DataService snapshots, so DataService
stays next. When charting's turn comes, brainstorm and spec the chart module
with this spike's routing rule and gpuchart's core layering as starting
constraints.
