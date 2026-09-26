# Geode codebase review — 2026-09-25

Reviewed at `main` = fe548817. Fifteen independent read-only passes, each verified against code with
file:line citations, plus a coordinator pass that re-checked every headline finding by hand.

This is a snapshot, not a live defect list. Severities and line references age with the code; treat
each finding as a claim to re-verify. Findings acted on are traceable through the spec that cites them,
starting with `docs/superpowers/specs/2026-09-25-geode-harness-and-mechanical-fixes-design.md`.

Start with **SYNTHESIS.md**: the measured baseline, the five cross-cutting themes, the confirmed
critical findings, and the per-area summaries. The other files are the full detail for one area each.

| File | Area | Lines |
|---|---|---:|
| `SYNTHESIS.md` | Cross-cutting themes, critical list, per-area summaries | 429 |
| `architecture.md` | Dependency graph, module contract, philosophy conformance, next-phase levers | 812 |
| `performance.md` | Render-path allocation census, benches vs budgets | 755 |
| `tests.md` | Test inventory, mutation harness audit, CI gaps | 852 |
| `docs-clarity.md` | Documentation drift, comment hygiene, naming, dead code | 572 |
| `core.md` | geode-core | 778 |
| `data-service.md` | geode-data service, ingest, health, storage | 151 |
| `data-query.md` | geode-data query compiler and SQL contract | 607 |
| `shell-state.md` | Tiling, keymaps, session, config writes | 683 |
| `shell-interaction.md` | ShellView, input, palette, frame state | 684 |
| `shell-dialogs.md` | Object dialog, dialog framework, choice lists | 755 |
| `blotter-diagnostics.md` | geode-blotter, geode-diagnostics | 919 |
| `marketdata.md` | geode-marketdata, geode-documents, egress | 594 |
| `pricer.md` | geode-pricer, geode-pricing, the request seam | 691 |
| `timeseries-chart.md` | geode-timeseries, geode-chart, geode-widgets | 454 |
| `app-demo.md` | geode-app, geode-demo-data, builtin config | 737 |
| `coordinator-notes.md` | Baseline measurements, pedantic clippy census, spot-checks | 11 |

Nothing here was implemented, and no file outside this directory was modified by the review itself.

## Reading order if you have limited time

1. `SYNTHESIS.md` sections "Baseline", "The five themes", "Confirmed critical findings".
2. `architecture.md` section (b) — the dependency verdict table and the TileContent implementor table.
3. The area file for whatever you are about to touch next.

## How the review was run

Fifteen subagents, one per area, each given the philosophy, CLAUDE.md, the relevant current guides and
the GPUI coding guides, and each required to cite file:line for every finding and to label anything it
could not confirm. The coordinator measured the baseline, re-verified each headline finding in code,
and wrote the synthesis. `coordinator-notes.md` holds the baseline numbers and the pedantic-clippy
census, which no single area pass owned.
