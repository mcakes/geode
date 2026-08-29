# The Geode Philosophy

Geode is the everything-tool for an index exotic equity derivatives desk: risk,
pricing, execution, and data visualization in a single, permanent, keyboard-driven
shell. This document is the charter that governs its design. Every feature,
module, and code review is judged against these principles. When a proposal
conflicts with this document, either the proposal changes or — deliberately,
and in writing — this document does.

## 1. Geode is a lens, not a brain

All intelligence — pricing models, risk engines, trade capture — lives upstream.
Geode's job is to make the outputs of those systems visible, navigable, and
actionable faster than any other tool on the desk. The only computation it
performs is *view-shaping*: grouping, filtering, aggregating, and joining data
that upstream systems produced.

The moment a number's correctness requires financial reasoning — a vol surface
interpolation, a PnL attribution, a greek — it belongs upstream. This line is
the load-bearing wall of the design. Every future "couldn't the app just
quickly calculate X" gets judged against it.

## 2. The keyboard is the interface

Every action reachable by mouse must be reachable by keyboard; the reverse is
not required. The trader's hands never leave home row: i3-style tiling window
management, vim-style modal navigation within tiles, and a command palette as
the universal, discoverable fallback.

Muscle memory is an asset we build deliberately. Keybindings are stable,
mnemonic, and user-remappable — and never churned casually. A binding that has
shipped is a promise.

## 3. Latency is a feature, silence is a bug

Every interaction responds within a frame budget. Nothing the data layer does
may ever stall the render thread — not an ingest, not a query, not a slow
network share.

When data *is* slow — a cold load, a degraded source — the UI says so honestly:
staleness, load progress, and source health are always visible. A trader must
never wonder whether a number is current. Known-stale data, clearly marked, is
acceptable; ambiguity never is.

## 4. One tool, many panes

Geode grows by adding modules to a permanent shell, not by spawning sibling
apps. The shell — windowing, keyboard, palette, configuration, data service —
is the product; modules are guests that obey its rules.

A module never invents its own navigation idiom, its own data-fetch path, or
its own configuration mechanism. Uniformity across modules is not a style
preference; it is what makes the tool learnable once and usable forever.

## 5. Config is data, shared by default

Views, layouts, queries, keymaps, and data sources are declarative text files —
diffable, hand-editable, and layerable: built-in defaults, then desk-level
defaults from a shared location, then personal overrides. A new view of
existing data should cost a config file, not a release.

What one trader builds, the desk can inherit.

## 6. Performance is a discipline, not an optimization pass

Geode is engineered like a game engine, not a CRUD app. Data-oriented design
throughout: struct-of-arrays over object graphs, columnar data flowing
end-to-end without materializing into row objects, cache-friendly iteration
over pointer-chasing.

Allocation is treated as a cost: hot paths are allocation-free, buffers are
reused across frames and refreshes, and per-frame heap churn is a reviewable
defect. We measure before and after: frame-time and query-latency budgets are
stated in specs, enforced with benchmarks, and regressions are treated as bugs.

Idiomatic Rust that is slow loses to slightly unusual Rust that is fast — with
a comment explaining why.
