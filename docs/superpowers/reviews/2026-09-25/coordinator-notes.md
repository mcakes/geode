# Coordinator's own observations (to fold into synthesis)

Baseline (main fe548817, 2026-09-25): fmt clean; clippy -D warnings clean; cargo test --workspace 3666 passed / 0 failed / 1 ignored; mutation anchors 1575 checked, 0 stale, 0 ambiguous. Future-incompat: `block v0.1.6` (transitive, via gpui/cocoa) will be rejected by a future rustc.

Pedantic+nursery clippy: 4441 warnings. Distribution by crate: shell 1370, data 670, core 645, pricer 341, marketdata 317, timeseries 292, chart 264, blotter 207, app 177. Top classes: use_self 1023, too_long_first_doc_paragraph 413, option_if_let_else 339, redundant_closure 294, semicolon_if_nothing_returned 230, needless_pass_by_ref_mut 165 (mostly tests), redundant clone 160 (bridge.rs 20, objectdialog/render.rs 16, dialog.rs 16, scope_sql.rs 14, service.rs 10), cast sign/precision ~300 (session.rs 9, query/catalog.rs 7, store/catalog.rs 4, chart decimate/time), float strict-eq 108 (nearly all in tests — the non-test ones are in geode-pricing MockPricer), significant_drop_tightening 34 (all short scopes on inspection; none held across I/O), match_wildcard_for_single_variants 14 (shorthand.rs, series/expr.rs, pricing.rs — a future variant would silently take the wildcard arm), match_same_arms 32.

Spot-checks:
- geode-data/src/handle.rs:218-227 replace_views takes `tx` lock then `pending_views` lock — verify no path takes them in the other order (service side reads pending_views; check it never touches tx while holding it).
- geode-data/src/ingest/runner.rs:200-210 IngestHandle::submit re-sorts the entire queue under the lock on every submit (O(n log n) per submit; fine at current sizes, note for the daemon plan).
- Comments citing spec sections exist in production code: geode-data/src/query/pool.rs:238 "(spec §7.3)"; geode-pricer/src/core/edit.rs:703 "(spec §6.4)" — CLAUDE.md forbids this.
- Stale worktrees: /private/tmp/geode-catalog-worktree and /private/tmp/geode-invalidation-worktree are ~100 commits behind main with nothing unmerged; .claude/worktrees/command-line-locality is merged too. Candidates for `git worktree prune`/removal. update-docs branch has 2 commits (docs only) not on main.
