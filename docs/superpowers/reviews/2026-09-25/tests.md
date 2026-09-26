# Verification review — test suites, mutation harness, benches, CI

Scope: the verification story across the workspace. Read-only review. Every
finding below was verified by reading the cited code; nothing was compiled or
run except `zsh scripts/mutation-check.sh --anchors-only` (0.185 s wall, no
cargo — confirmed by reading the script's `anchors_only` early-return at
`scripts/mutation-check.sh:212-215` and the final python pass at the tail).

Result of that run: `checked 1575 anchors: 0 stale, 0 ambiguous`.

---

## (a) Inventory

Test counts are attribute occurrences (`grep -c`) across each crate's `*.rs`,
including inline `#[cfg(test)]` modules. Mutation entries were extracted by
sourcing the entry section of `scripts/mutation-check.sh` under a stubbed
`run_mutation`, so the counts are the harness's own.

| Crate | src lines | pure `#[test]` | `#[gpui::test]` | total | mutation entries | ratio mut/test | bench files |
|---|---:|---:|---:|---:|---:|---:|---:|
| geode-shell | 93,045 | 1,072 | 594 | 1,666 | 612 | 0.37 | 1 |
| geode-data | 35,014 | 536 | 0 | 536 | 279 | 0.52 | 5 |
| geode-marketdata | 26,071 | 149 | 230 | 379 | 200 | 0.53 | 1 |
| geode-core | 17,682 | 356 | 0 | 356 | 143 | 0.40 | 2 |
| geode-pricer | 15,203 | 99 | 102 | 201 | 73 | 0.36 | 1 |
| geode-blotter | 10,617 | 74 | 48 | 122 | 90 | 0.74 | 1 |
| geode-timeseries | 10,732 | 49 | 55 | 104 | 48 | 0.46 | 1 |
| geode-app | 7,717 | 63 | 28 | 91 | 55 | 0 |
| geode-chart | 3,542 | 45 | 6 | 51 | 19 | 0.37 | 1 |
| geode-diagnostics | 3,346 | 29 | 28 | 57 | 27 | 0.47 | 0 |
| geode-documents | 2,754 | 46 | 0 | 46 | 13 | 0.28 | 2 |
| geode-demo-data | 2,130 | 26 | 0 | 26 | 4 | 0.15 | 2 |
| geode-widgets | 1,419 | 31 | 1 | 32 | 10 | 0.31 | 0 |
| geode-pricing | 326 | 8 | 0 | 8 | 2 | 0.25 | 0 |
| **total** | **229,598** | **2,583** | **1,092** | **3,675** | **1,575** | 0.43 | **19** |

Other axes:

| Axis | Value |
|---|---|
| Mutation entries | 1,575, every one naming a test (0 without a `test_filter`) |
| Distinct (package, test) named by entries | 1,356 |
| Criterion bench targets | 19 files; all `harness = false`, every lib `bench = false` |
| Integration test files (`tests/`) | 2, both in geode-shell (280 lines, 3 tests) |
| proptest users | geode-data (2 files), geode-marketdata (2), geode-chart (1), geode-documents (1) — 6 `proptest!` blocks total |
| `#[ignore]` tests | 0 |
| `#[should_panic]` | 3 |
| `debug_bounds(...)` assertions in tests | 602 against 175 `debug_selector` sites in src |
| Shell test dir | 23 files, 30,146 lines; largest `objectdialog.rs` 9,436, `chrome_and_dialogs.rs` 2,820 |
| CI jobs | 1 (`check`) × 2 OS (macos-latest, windows-latest), 6 steps |
| `zsh scripts/mutation-check.sh --anchors-only` | 0.185 s, exit 0 |

Shell test files, window vs pure (all 23 files are `#[gpui::test]`-only; the
pure cores are tested inline beside their modules instead):

| File | gpui tests | key/pointer sims | direct `shell.update` |
|---|---:|---:|---:|
| objectdialog.rs | 177 | 485 | 16 |
| chrome_and_dialogs.rs | 59 | 101 | 6 |
| keybindings_dialog.rs | 54 | 120 | 5 |
| occupants.rs | 33 | 40 | 16 |
| drag.rs | 29 | 59 | 1 |
| reload.rs | 28 | 10 | 1 |
| palette.rs | 25 | 69 | 0 |
| asof.rs | 20 | 42 | 2 |
| stacks.rs | 18 | 23 | **19** |
| scopebar.rs | 18 | 42 | 2 |
| (13 more) | 90 | 218 | 12 |

Pure-core coverage lives next to the cores: `tiling/tree.rs` 101 tests,
`tiling/workspaces.rs` 108, `tiling/dropzones.rs` 14, `tiling/dividers.rs` 13,
`tiling/docks.rs` 12; `session.rs` 56; `config_write.rs` 7.

**Thin crates.** `geode-widgets` (1 window test for a crate that exists to hold
reusable widgets), `geode-pricing` (8 tests, 0 mutations beyond 2, no bench
despite being the first in-process calculation leaf), `geode-demo-data`
(ratio 0.15 — the fixture generator every data test depends on),
`geode-diagnostics` (no bench file though it renders a large table),
`geode-documents` (0.28, and it owns the parse/write round-trips that upload
echo compares against).

---

## (b) Findings

### Critical

**C1. One mutation entry names a test that does not exist, and the harness
silently falls back to the whole crate suite — reporting `caught` on the
strength of unrelated tests.**
`scripts/mutation-check.sh:18015-18019` names
`a_commit_whose_line_went_away_is_refused`; the only similar function in
geode-pricer is `crates/geode-pricer/src/tile.rs:3825`
`an_editor_whose_line_went_away_closes_with_moved`. I enumerated every `fn`
name in each entry's package and matched all 1,575 filters: this is the only
entry whose filter matches no function at all. The script handles this
(`scripts/mutation-check.sh:287-292`): it prints `FILTER … matches no test`,
then clears the filter and runs the full `-p geode-pricer --lib` suite for a
plain caught/SURVIVED verdict. So the entry still prints `caught` — from any
of 201 geode-pricer tests — which is precisely the "two defences overlapping"
lie the script header warns about at lines 100-110. `--anchors-only` cannot see
this because it only checks anchors, never filters.
*Impact*: the commit-refusal contract for a pricer line that vanished has no
named defence, and the harness reports otherwise.
*Direction*: extend the `--anchors-only` pass to also validate that each
filter matches at least one `fn <name>` in its package's sources (a regex over
the same already-read file texts, near-free); fix this entry's name.

**C2. `cargo test -- <filter>` is a substring match, so 11 entries' verdicts
can come from a different test than the one named.**
The script builds `cargo test -p "$pkg" $target_flag -- "$filter"`
(`scripts/mutation-check.sh:286`) with no `--exact` (`grep -c '--exact'` = 0).
Eleven filters are proper substrings of other test names in the same package,
so both run and either can produce the failure the script reads as "caught by
the named test". Verified pairs include
`escape_walks_the_ladder_one_rung_at_a_time` vs
`settings_escape_walks_the_ladder_one_rung_at_a_time`;
`emits_config_reloaded_before_the_frame_notifies` (a bare substring that
matches only *two other* functions — `crates/geode-shell/src/shell/tests/reload.rs:1410`
`a_dimensions_change_that_resolves_a_grouping_slot_still_emits_config_reloaded_before_the_frame_notifies`
and `a_views_change_emits_config_reloaded_before_the_frame_notifies_its_observers`
— and no function of its own name);
`row_motion_is_counted_and_clamped` vs `…_but_a_bare_step_wraps`;
`patch_cell_matches_a_rebuild` vs `…_under_a_pivot` / `…_with_rows_spliced`;
`outcome_after_a_key_switch_is_a_notice_naming_the_key` (again names no function
of its own, matches `an_ok_…` and `an_err_…`);
`space_scrolls_the_next_row_into_view` vs `shift_space_…`;
`x_leaves_the_cursor_on_the_next_row` vs `…_still_in_view`;
`an_explicit_direction_beats_the_setting` vs `…_and_lands_where_it_says`;
`escape_puts_back_the_query_filter_mode_was_entered_with` vs `settings_…`
(twice — that name is also one of the two duplicated helper names across shell
test files).
*Impact*: for these entries the "named test" claim, which the script's own
header calls the reason every entry names a test, is not established. Four of
them (the `substring-only` set) name no function at all, so the claim is
strictly false there.
*Direction*: add `--exact` and pass the module path, or accept substrings but
have `--anchors-only` report a filter that matches more than one `fn`, exactly
as it reports an ambiguous anchor.

### Major

**M1. 45 entry pairs share a (file, anchor) and 3 share (file, from, to)
verbatim, so `replace(..., 1)` makes the second entry of each pair mutate the
same first occurrence — two entries defending one line.**
The script mutates only the first match (`scripts/mutation-check.sh:278-282`)
and reports `AMBIG` when an anchor occurs more than once *in the file*; it does
not notice when two *entries* carry the same anchor. Three pairs are fully
identical (file + from + to), verified by extraction:
`crates/geode-data/src/ingest/runner.rs` — "ingest: the runner re-checks change
detection at pop time" and "ingest: a stale skip does not start the strip",
both naming `a_queued_item_whose_file_was_loaded_meanwhile_is_skipped_at_pop_time`;
`crates/geode-core/src/colour/mod.rs` — "colour: the readability floor pulls
lightness until 3:1" and "theme: bundled themes clear 3:1 through the
resolver"; `crates/geode-marketdata/src/core/upload.rs` — "upload: echo
compares rows as a multiset" and "upload: an out-of-order insert confirms
through the real store". The last two pairs at least name *different* tests, so
they read as "two independent defences of one line", which is defensible; the
runner pair is the same mutation and the same test twice, i.e. one entry's
information reported twice, inflating the count.
*Impact*: the entry count overstates distinct guarded behaviours (1,575 entries
name 1,356 distinct (pkg, test) pairs).
*Direction*: have `--anchors-only` also group by (file, from, to) and report
exact duplicates; keep deliberate same-anchor/different-test pairs but make
them explicit in the entry name.

**M2. 43 same-file anchor pairs where one anchor is a substring of another —
the "shorter-indent anchors are substrings of longer ones" hazard is live, and
only luck (first-match ordering) keeps it correct.**
Verified by extraction: within one file, 43 ordered pairs (A, B) exist with
A ≠ B and A contained in B. Examples: in
`crates/geode-data/src/query/as_of.rs`, the anchor
`from generations where dataset = ? and source_time <= ?` is a substring of a
longer anchor in the same file; in `crates/geode-data/src/store/catalog.rs`,
`and fg.batch = ? and fb.book is null`; in
`crates/geode-data/src/query/scope_sql.rs`,
`|| ds.column(base).and_then(|c| c.grain()) == Some(grain)`. Because
`str.count()` (the `hits` check) and `str.replace(..., 1)` both work on the raw
text, a short anchor inside a longer construct still counts 1 if the enclosing
text occurs once — it is not flagged, and it mutates the enclosing site, which
may be the intended one or not. This is the exact failure the memory note
records from the tile-picker work.
*Impact*: an edit that duplicates the *enclosing* construct silently moves a
short anchor's target without tripping AMBIG.
*Direction*: add a minimum-anchor-length or a "contains a `\n`" preference for
new entries, and report substring-of-another-anchor pairs in `--anchors-only`
as an advisory count (it already has every file's text in memory).

**M3. 14 anchors are shorter than 20 characters; several are generic enough
that an ordinary refactor elsewhere in the file would capture them.**
Verified by extraction. Among them: `'where not exists ('`
("generations: the publish insert dedup guard is dropped"),
`'if matches!(ch,'` ("scope: LIKE wildcards escaped"), `'    if already {'`,
`'    if text_entry {'`, `'    if !in_browse {'`, `'    if t <= 0.0 {'`,
`'} else if behind {'`, `'.create_new(true)'`, `'tag != outcome.tag'`,
`'"pricing.adapter"'`. Each currently counts exactly 1 (the anchors-only run is
clean), so none is broken today; the risk is that a new `if already {` or a
second `where not exists (` in the same file silently redirects the mutation to
the wrong site while still printing `caught`.
*Impact*: latent, quiet mis-targeting; the AMBIG report catches the
duplicate-count case but only after it happens, and only for selected entries
in a normal run.
*Direction*: lengthen these anchors to include the surrounding line or the
function signature.

**M4. `stacks.rs` proves stack behaviour almost entirely through
`shell.dispatch(ActionId(...))` rather than keys — 19 direct `shell.update`
blocks against 23 key simulations, the highest ratio in the suite.**
`crates/geode-shell/src/shell/tests/stacks.rs:27,120,132,176,214,235,247,268,
280,293,307,321,377,404,429,457,487,524,594`. The fixture itself
(`stacks.rs:20-40`) presses `ctrl-v` twice for the first two tiles, then
dispatches `tile::add_rec_stacked` directly; tests such as
`a_count_prefix_steps_n_members` (`stacks.rs:115-140`) dispatch
`stack::next` with `Some(2)` as the count rather than pressing a counted key.
CLAUDE.md's "Test production routes. Calling an internal mutation does not
prove that a key, pointer event, delivery, or focus transition reaches it." is
satisfied at the action layer but not at the keymap layer: nothing here proves
`mod+[`/`mod+]` (the bindings the tile-stacks handoff records) resolve to these
action ids in the shipped keymap, nor that a count prefix typed at the keyboard
reaches `dispatch`'s count parameter.
*Impact*: a binding regression or a count-parsing regression in the stack
vocabulary would leave all 18 stack tests green.
*Direction*: convert at least one test per verb to `simulate_keystrokes`, or
add a keymap-resolution assertion (the pattern `tests/keymap_integration.rs`
already demonstrates) covering the stack action ids.

**M5. The `--changed` everyday mode cannot see an entry whose *test* changed —
only whose *mutated file* changed.**
`scripts/mutation-check.sh:203-206` skips any entry whose `file` (field 2, the
mutation target) is absent from the changed set; the changed set is
`git diff --name-only <ref>` + working tree + untracked
(`scripts/mutation-check.sh:184-192`). An entry's *test* lives elsewhere for
every geode-shell entry whose code is in `src/shell/**` and whose test is in
`src/shell/tests/**` — 612 geode-shell entries, and the top mutated files
(`objectdialog/render.rs` 96 entries, `objectdialog/mod.rs` 74) are all
guarded by tests in `src/shell/tests/objectdialog.rs`. So a change that
weakens or deletes an assertion in a test file, with the production file
untouched, is invisible to `--changed`: the entry is skipped, and the merge
gate (`--anchors-only`) checks anchors only.
*Impact*: the harness's stated purpose — "verifies that a named test detects a
specific broken behavior" — has a blind spot exactly where tests are edited,
which is where the UTF-8 incident's corrupted-expectations failure lived.
*Direction*: treat an entry as selected when *either* its mutated file or a
file containing its named test has changed (a grep for the filter name over the
changed set is enough).

**M6. No automated guard against expectation corruption beyond one file.**
The UTF-8 double-encoding incident produced exactly one guard:
`crates/geode-blotter/src/delegate.rs:1198-1224`
`tree_glyphs_are_the_code_points_the_design_names`, which spells `\u{25B8}`,
`\u{25BE}` and `\u{2020}` as ASCII escapes so "an escape cannot be
re-encoded". That is the right mechanism, but it covers three code points in
one file. 106 source files outside test directories still contain non-ASCII
string literals (`crates/geode-shell/src/palette.rs`, `commandline.rs`,
`choice.rs`, `diagnostics.rs`, `reload.rs`, `vimfind.rs`, `scopebar.rs`,
`module.rs`, `footer.rs`, `frame.rs`, `defaults.rs`, `shell/chip.rs`, …), and
`grep -rn '\\u{25…' | grep assert|const` finds no other escape-spelled guard.
There is no repository-wide Latin-1 round-trip scan (the memory note says one
was added; `ls scripts` shows only `mutation-check.sh`, and CI has no such
step).
*Impact*: a recurrence in any of those 106 files reproduces the original
failure mode — corrupted glyphs, green suite, because both sides are corrupted
identically.
*Direction*: a ~20-line test (or a `scripts/` scan wired into CI) that reads
every `crates/**/*.rs`, re-encodes each file's text through Latin-1 and fails
on any file whose bytes round-trip — the memory note's own prescription, made
workspace-wide and cheap.

**M7. `perf.rs` sleeps 20 ms of real wall-clock inside a `#[gpui::test]`.**
`crates/geode-shell/src/shell/tests/perf.rs:147`
`std::thread::sleep(std::time::Duration::from_millis(20))` inside
`reset_drops_the_previous_render_timestamp_so_the_first_sample_after_it_is_fresh`
(`perf.rs:127-182`). The comment explains the intent well — the gap stands in
for a user's reaction time, and `FrameHistogram` reads real `Instant`s, not the
test clock — but the assertion `perf.count() == 0` depends on the *production*
code having cleared `last_render_started`, while the assertion's
distinguishing power depends on 20 ms being long enough to land in a non-zero
bucket on a loaded CI runner. It is the only real sleep in the entire test
suite (`grep -rln 'thread::sleep'` over `crates` returns this file plus
production modules only).
*Impact*: a slow shared runner makes this the suite's most likely flake; a fast
one could in principle make 20 ms indistinguishable from the idle cutoff.
*Direction*: inject a clock into `FrameHistogram` (the app already has
`geode_core::clock::Clock` and an `AppClock` global for exactly this) so the
gap is simulated, and keep the sleep only if a real `Instant` is genuinely
unavoidable.

**M8. Sub-second negative waits: seven tests assert "nothing arrives" by
waiting 100–200 ms.**
`crates/geode-shell/src/config_write.rs:465`
(`recv_timeout(Duration::from_millis(100))`, asserting the second edit has
*not* entered);
`crates/geode-data/src/egress.rs:384`;
`crates/geode-data/src/adapter/channel.rs:446,452,557,603`;
`crates/geode-data/src/service.rs:4100`;
`crates/geode-data/src/ingest/scheduler.rs:833` (200 ms). Each asserts
`.is_err()` — "the timeout expired, so nothing came". On a contended Windows
runner a slow-but-correct producer looks identical to a correct silence; and
because these are *negative* assertions they fail in the safe direction only
by accident (a late arrival after the window passes leaves the test green
having proved nothing).
*Impact*: both flake risk and silent weakening. The `config_write.rs` one
guards a real correctness contract (ordered user-layer writes — CLAUDE.md's
"Runtime config writes … go through `geode_shell::config_write`").
*Direction*: where the producer is in-process, use an explicit barrier or a
"the first edit has committed" signal rather than a duration; where a duration
is unavoidable, state the assumption in the failure message.

**M9. Wall-clock `SystemTime::now()` / `Instant::now()` inside tests of
time-ordered state.**
`crates/geode-shell/src/shell/tests/diagnostics.rs:56,88,138,505` pass
`SystemTime::now()` as a health report's change stamp; the contract under test
("Equal severities choose the most recently changed health/detail pair" —
`docs/current/data-path.md`, Freshness/health section) is about *ordering*
stamps, and two `now()` calls in the same test can land on the same instant on
a coarse-clock platform (Windows' default timer granularity is ~15.6 ms).
Similarly `crates/geode-shell/src/shell/tests/flip.rs:80-82,165-167` and
`crates/geode-timeseries/src/tile/tests.rs:144,372,1318,1370` build deadlines
from `Instant::now() ± 1ms`. `flip.rs:161` is candid about why
(`Frame::sweep` reads real `Instant::now()`).
*Impact*: platform-dependent flakes in exactly the lanes whose whole point is
ordering, and a contract proven only for "stamps that happen to differ".
*Direction*: pass explicit distinct stamps (`t0`, `t0 + 1s`) — the tests do not
need real time, only ordered time.

**M10. `geode-widgets` has 32 tests and 1 window test for a crate created to
be the shared widget layer.**
`crates/geode-widgets/src` is 1,419 lines with `pure=31 gpui=1` and no bench.
The as-of dialog handoff records that this crate was extracted precisely so the
segmented date row and the `[time]` clock could be reused. A widget's contract
is pointer/keyboard/focus behaviour, which one window test cannot cover; the
guides' own list of failure modes includes "tests that call internal methods
but never exercise keyboard or pointer behavior"
(`.claude/skills/gpui-kit/references/coding-guides.md`, Common failure modes).
*Impact*: the reusable layer is the least protected against an interaction
regression, and it now has at least two consumers.
*Direction*: one window test per widget covering the documented pointer +
keyboard route; 10 mutation entries already exist and would become meaningful.

**M11. No bench covers the 8 ms pure-UI budget the docs state; the 50 ms
requery budget is covered, the 2 ms chart budget is covered.**
`docs/current/performance.md` states three numeric budgets. Traced to benches:
requery — `crates/geode-data/benches/query.rs:419-520`, which really does run
`100_000` and `1_000_000` rows (`query.rs:423`) through `requery` including
as-of variants (12 `as_of` references); chart — `crates/geode-chart/benches/
decimate.rs:57-70` `decimate/500k_into_1600` and `decimate_and_path/…`. But
"Pure UI action under 8 ms" has no bench that measures a UI *action*: the
closest, `crates/geode-shell/benches/shell_cores.rs`, benches
`bench_tree_layout`, `bench_divider_strips`, `bench_dropzones`,
`bench_matcher`, `bench_palette` — five pure cores, none of them an action
round trip (key → action → state → prepared model). The doc itself concedes
the whole-frame gap ("A real painted frame is not covered by headless Criterion
benchmarks", `performance.md:106-108`) but not this narrower one: the 8 ms
number has no measuring instrument at all, only the `FrameHistogram`
observability path.
*Impact*: the most-cited budget in the repo is unfalsifiable by the bench
suite; a regression is detectable only by a human noticing a slow frame.
*Direction*: bench the composed path that the histogram observes — e.g. build
the prepared model for a blotter action (`ColumnPlan::build` + flatten is
already benched in `blotter.rs`; what is missing is the shell-side dispatch and
frame recompute) and state it against 8 ms.

**M12. CI does not run the merge gate the project's own rules require.**
`.github/workflows/ci.yml` has six steps: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `cargo bench --workspace --no-run`,
`cargo check -p geode-shell --features test-support --all-targets`. CLAUDE.md
says "Run `--anchors-only` before merge"; the script's own header says
"Exits non-zero on any finding, so it can gate a merge … Run it before every
merge". It is not in CI. It costs 0.185 s and needs no cargo — I measured it.
Also missing: `cargo-deny`/`cargo audit` (no `deny.toml` in the repo), an MSRV
job (`rust-toolchain.toml` pins only `channel = "stable"`, so the workspace's
supported Rust floor is unstated and untested), and `cargo test --doc` is only
nominally exercised — there is exactly 1 non-`text`/`ignore` doc code block in
the whole workspace, so doc tests prove nothing today.
*Impact*: the anchors gate depends entirely on a human remembering, and the
memory notes record that nine duplicate anchors once shipped before the gate
existed.
*Direction*: add a third job (ubuntu, no cargo needed) running
`zsh scripts/mutation-check.sh --anchors-only`; add `cargo deny check` and a
pinned-MSRV `cargo check`.

**M13. `cargo test --workspace` on Windows runs the same tests as macOS with
no platform-specific coverage, yet the keymap has a documented platform
branch.**
CI's matrix is `[macos-latest, windows-latest]`
(`.github/workflows/ci.yml:14-16`), which is good. But the only
platform-conditional test code in the workspace is
`crates/geode-shell/src/shell/tests/palette.rs:673` / `:679`
(`#[cfg(target_os = "macos")]` / `#[cfg(not(target_os = "macos"))]`), and
`grep -rn 'cfg(unix)\|cfg(windows)'` over `crates/*/src` returns nothing — so
`default_mod()`'s platform behaviour is asserted in one place. Meanwhile the
platform-sensitive *production* surfaces are the ones most likely to differ:
atomic file replacement (`config_write.rs`'s temp-then-rename,
`session.rs`'s atomic replace), path handling in discovery globs, and the
`$TMPDIR`-based demo store. None has a Windows-specific assertion.
*Impact*: Windows CI green means "compiles and the platform-agnostic tests
pass", which is weaker than it reads.
*Direction*: no new job needed — add path/rename assertions that differ by
platform where the behaviour genuinely differs, and mark the rest as
deliberately platform-agnostic.

**M14. Two documented silent-wrong-data contracts have no test I can find.**
Sampling 21 concrete claims from `docs/current/data-path.md` and grepping for
covering test names, 19 are covered (often well — see (d)). Two are not:

- *"A same-size correction with the same source time is therefore skipped, even
  if its bytes changed. Discovery and the runner's pre-load check share this
  rule."* (`docs/current/data-path.md`, Source discovery section.) Discovery has
  `is_unchanged_true_only_when_size_and_source_time_both_match`
  (`crates/geode-data/src/source/discovery.rs:411`) and
  `an_already_loaded_unchanged_file_is_skipped` (`:322`) — both prove the
  *skip*, neither constructs the dangerous case: same size, same source time,
  **different bytes**. `grep` for `same_size|bytes_changed|content_hash` across
  geode-data returns 0 test functions. This is the documented data-loss path
  (a correction that is silently ignored), and it is the one the doc
  specifically calls out.
- *"Series fetch completion is broadcast to visible occupants by (identity,
  source) … including when a fetch appended zero rows."*
  (`docs/current/data-path.md`, Freshness/health/delivery.) The store side is
  covered — `an_empty_fetch_records_coverage_and_appends_nothing`
  (`crates/geode-data/src/store/series.rs:550`) — but the *broadcast* side is
  not: `crates/geode-data/src/handle.rs:198` documents "when completion
  appended zero rows" in a doc comment, and the service's fetch tests
  (`service.rs:2600` `a_fetch_lands_rows_and_announces_the_pair`, `:2645`,
  `:2676`, `:2691`, `:141` failure lane) all announce with rows. Nothing
  proves a zero-row completion still reaches a watching module — the exact
  case where a timeseries tile would otherwise spin on "loading" forever.

*Impact*: both are silent-wrong-state failures; the first is silently stale
data, the second a stuck UI.
*Direction*: two tests, each ~20 lines, plus a mutation entry each.

**M15. `objectdialog.rs` is 9,436 lines with 177 window tests, 119 commits of
churn, and no internal module structure.**
`grep -n '^mod \|^    mod '` on it returns nothing — it is one flat file. Its
441 `cx.run_until_parked()` calls, 155 `tempfile::tempdir()`, 109
`simulate_keystrokes("enter")`, 52 `dialog_test_shell_in_dir(…)` and 39
`open_tree_edit_stage(cx, dir.path())` show a heavily-repeated prologue that is
already partly factored (the two helpers) but not consistently. The memory note
"sonnet stalls holding a 10k+-line file; split moves into parts" records the
operational cost directly.
*Impact*: the file is at the size where agents stall and humans stop reading
neighbours before adding a test — which is how duplicated, drifting fixtures
appear. Two helper names are already duplicated across shell test files
(`escape_puts_back_the_query_filter_mode_was_entered_with` and
`clearing_the_query_clears_the_field_the_next_filter_session_sees` each defined
twice), and C2 shows one of those duplicates corrupting a mutation filter.
*Direction*: split by domain along the lines the names already cluster on
(view 26, column 23, scope 22, schema 9, source 9, dataset 8, grouping 4) into
`tests/objectdialog/{view,column,scope,schema,source}.rs`, moving the shared
prologue into that directory's `mod.rs`.

### Minor

**m16. 34 mutation entries change only a string literal, which makes them copy
guards rather than behaviour guards.**
Verified by extraction (both `from` and `to` begin with `"`). Examples:
"dialog: the frozen empty filter shows its placeholder"
(`"press / to filter"` → `""`, naming
`the_keybinding_dialogs_pill_sits_in_the_title_row`); "keybindings: d promises
the retired retype recovery" (`"press r to restore it"` → a different
sentence); "retention: age keeps the recent, not the ancient" (rewrites the
`source_time >= …` SQL fragment). Several are genuinely load-bearing (the SQL
fragments, and the field-help memory note records that "help copy is a
behaviour claim"), but a copy guard pinned to an exact sentence is the kind of
entry that breaks on every wording change and teaches people to edit the
harness rather than think.
*Direction*: keep the SQL ones; for user-facing copy, assert the *claim* (does
the footer name a live key?) rather than the sentence — `RowVocabulary` already
makes that possible.

**m17. `dialog_test_shell_in` dispatches the opening action directly, so no
dialog test proves its own opening route.**
`crates/geode-shell/src/shell/tests/mod.rs:466-507`: the fixture opens the
window, draws, then
`shell.dispatch(&ActionId(action.to_string()), None, window, cx)`. Every one of
the ~290 dialog tests therefore starts from "the action fired", never from "the
key or the click that fires it". The fixture is otherwise exemplary — it calls
`dialog::init_reclaimed_keybindings` with a comment explaining that without it
"a dialog test that presses tab would prove nothing", which is precisely the
right instinct.
*Direction*: one test per dialog that reaches it by its shipped binding or its
palette row; the palette tests (`palette.rs:222`
`ctrl_k_opens_types_filters_and_enter_dispatches_the_selected_action`) show the
shape.

**m18. Mouse-opened-dialog typing discipline is documented in comments, not
enforced.**
The grouping-picker memory note records the mechanism bug ("a mouse-opened
dialog test must TYPE after the click") and three comments cite it:
`crates/geode-shell/src/shell/tests/grouping.rs:68`,
`crates/geode-shell/src/shell/tests/scopebar.rs:303` and `:320` (the last
explains `prevent_default` is the fix). Only three sites. Of the mouse-opened
dialog surfaces the handoffs list (grouping readout, tile picker placeholder,
empty dock, scope-bar chips, toolbar AS OF chip, swatch), I can confirm typing
after the click only at these.
*Direction*: a shared `open_by_click_and_type(...)` helper that clicks, then
types one character and asserts it landed — so every new mouse door inherits
the check instead of re-deriving it.

**m19. Fresh-session fixtures: the empty-tree case is covered, which is worth
recording as closed.**
The memory note warns "a fresh session has NO tile, test fixtures that only
make placeholders miss it". This is now covered:
`crates/geode-shell/src/shell/tests/tilepicker.rs:325`
`double_clicking_the_empty_tree_hint_opens_the_picker_and_a_pick_fills_the_tree`,
`:369` `a_single_or_modified_click_on_the_empty_tree_hint_opens_nothing`,
`:397` `showing_an_empty_dock_focuses_it_and_mod_n_adds_into_it`, `:440`
`clicking_an_empty_dock_focuses_it_and_double_clicking_adds_into_it`, `:143`
`a_docked_placeholder_double_click_opens_the_picker_and_fills_it`. But the
*default* fixture still presses `ctrl-v` to get a tile
(`crates/geode-shell/src/shell/tests/mod.rs:23-33`, `TEST_ADD_KEYMAP`), so the
empty case is exercised only where a test opts in.
*Direction*: none required; noted so it is not re-raised.

**m20. 15 helper functions are named `test_*`, the one naming pattern the suite
otherwise avoids.**
`crates/geode-app/src/bridge.rs:1099,1120,1132,1176,1228,1239,1510,1966`,
`crates/geode-app/src/demo.rs:333,351`,
`crates/geode-shell/src/shell/tests/palette.rs:1127`,
`crates/geode-shell/src/shell/tests/scopebar.rs:31`,
`crates/geode-shell/src/shell/keybindings_view.rs:1624`, plus
`crates/geode-data/benches/series_query.rs:125`. All are fixtures, not tests —
so no test is misnamed — but `test_services_with_ctrl_alias` reads like a test
in a grep and in a mutation filter. `crates/geode-shell/src/keymap/matcher.rs:178`
`simple_match` is the one genuinely uninformative *test* name I found across
3,675 tests.
*Direction*: rename the fixtures to `services_with_*` (the shell's own
convention already) and give `simple_match` a contract name.

**m21. `geode-shell/tests/` holds 3 legacy integration tests (280 lines) whose
role is unclear beside 30,146 lines of in-crate window tests.**
`crates/geode-shell/tests/keymap_integration.rs:13`
`keystrokes_drive_the_tiling_tree` is a genuinely valuable seam test — builtin
keymap → `Matcher` → `ActionId` → `apply_workspace_action` → `Tree` geometry,
with a header saying "This is the exact pipeline Phase 1b-ui wires into gpui's
key handler". But that pipeline is now wired through `ShellView::dispatch`, and
the file tests the pieces without the view. `tiling_integration.rs` has 2 more.
Neither is referenced by a mutation entry (all 612 geode-shell entries run
`--lib`, per `scripts/mutation-check.sh:210-213`'s `target_flag="--lib"`), so
**no integration test is ever mutation-checked**.
*Impact*: the one test that proves the keymap-to-tiling seam end to end is
outside the harness's reach.
*Direction*: either move these into the lib's test tree so `--lib` covers them,
or teach the harness `--tests` for the entries that should reach them.

**m22. `geode-pricing`, the first in-process calculation leaf, has 8 tests, 2
mutation entries and no bench.**
`crates/geode-pricing/src/lib.rs` is 326 lines, `pure=8 gpui=0`, no
`benches/`. `docs/PHILOSOPHY.md` §1 gives this crate a special constitutional
status ("a microservice that lives in our binary", reached "through the same
request-and-outcome door a remote service would use"). The door — request in,
keyed outcome back, no direct call from a module — is the invariant worth
proving, and `grep` shows `MockPricer` is what the app tests use
(`crates/geode-app/src/bridge.rs:3133`).
*Direction*: one test that the pricer is unreachable except through the
registry door (a compile-level or registry-level assertion), and a bench if any
real pricing arrives.

**m23. `geode-diagnostics` renders a large table and has no bench.**
`crates/geode-diagnostics/src` is 3,346 lines, `pure=29 gpui=28`, `benches/`
absent (verified by `ls crates/*/benches`). `docs/current/performance.md`
records a known gap for exactly this surface: "Diagnostics perf rows sample
requery and catalog resource metrics on their next rebuild; those inputs have
no dedicated perf invalidation. Histogram copying compares sample count and
maximum, so idle-only changes and a reset/refill with the same count and
maximum can be missed." The second sentence describes a *correctness* gap in
change detection, stated in the perf doc, with no test named for it (the
closest is `crates/geode-shell/src/diagnostics.rs:942`
`the_action_tail_keeps_the_last_thirty_two_without_allocating`).
*Direction*: a test for the count-and-maximum collision case (construct two
histograms with equal count and max but different distributions and assert the
copy is or is not taken, per the intended contract).

**m24. Display checks are tracked only in session memory, not in the repo.**
`grep -rn -i 'display check'` over `docs/current/*.md`, `TODO.md` and every
crate README returns three generic statements
(`docs/current/architecture.md:141`, `docs/current/features.md:407`,
`docs/current/shell.md:360`) — the *policy*, not a list. Yet the memory index
carries roughly two dozen "display check pending" items: line-number gutter,
groupings default landing, dialog mouse parity (drag gesture untestable in
`TestAppContext`), settings modal, Phase 4c 2b/2c, dataset column presentation,
dialog step-key footers, tint-sign triad, as-of dialog, command-line locality,
filter-exit footer, cursor stops, timeseries mouse, scopes dialog, tile stacks,
keybindings reset, market-data panel header, browse delete/revert, field help,
design-guide audit item 8, pricer flags. `docs/current/architecture.md:140-142`
says such a check "should be recorded as a limitation in the relevant current
guide"; that has not happened for these.
*Impact*: the pending-visual-verification backlog is invisible to anyone
without the memory files, and the project's own rule for recording it is
unmet.
*What could be automated*: a good fraction. The font-size test
(`crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs:2670-2822`) is proof
that geometry claims are headless-testable — it asserts the painted status bar
height, the painted sidebar rail width, the painted command-line strip height
*and* that the strip's bottom sits on the tile's bottom border, all through
`debug_bounds`. By that standard: footer copy and chip presence (already done
in places via `debug_bounds("dialog-mode-pill-chain")`), row-ground and
cursor-border geometry, gutter width, strip alignment, and every "is this
element painted at all" claim are automatable now. What genuinely is not:
exact colour (though the two theme sweeps show contrast *ratios* are
testable), animation, and real drag gestures (`TestAppContext` limit, recorded
in the dialog-mouse-parity note).
*Direction*: add a short "Pending display checks" list to
`docs/current/features.md` (or a `docs/display-checks.md`) so the backlog lives
with the code, and convert the geometry subset using the font-size test as the
template.

**m25. proptest is used in 6 places and none covers the tiling tree.**
`crates/geode-data/src/query/scope_sql.rs:1883,2062`,
`crates/geode-data/src/store/series.rs:949`,
`crates/geode-documents/src/cvi.rs:1453`,
`crates/geode-chart/src/core/decimate.rs:151`,
`crates/geode-marketdata/src/core/draft.rs:2076`,
`crates/geode-marketdata/src/core/matrix.rs:2360`. The `cvi.rs` one is a model
round-trip (`prop_assert_eq!(parsed.rows, rows)` at `:1520`) — exactly the
right use. `geode-shell`'s `tiling` module, the workspace's largest pure state
machine (101 + 108 + 14 + 13 + 12 = 248 example-based tests across
`tree.rs`, `workspaces.rs`, `dropzones.rs`, `dividers.rs`, `docks.rs`), has
none, and `geode-shell` has no proptest dependency at all.
*Direction*: see I2.

### Ideas

**I1. Extend `--anchors-only` into a full entry linter — it already reads every
file.**
The tail pass (`scripts/mutation-check.sh`, final python block) reads each
anchored file once and counts anchors. For free, in the same pass, it could
report: filters matching no `fn` (catches C1), filters matching more than one
`fn` (C2), duplicate (file, from, to) tuples (M1), anchors that are substrings
of another anchor in the same file (M2), and anchors under N characters (M3).
That converts five of this review's findings into a 0.2 s gate. Keep the exit
code semantics it already has.

**I2. Property tests for the tiling tree and the session round trip.**
The tiling tree has the shape property tests are made for: a sequence of
random operations (split, close, focus-move, resize, dock, stack, pop-out,
fullscreen) against invariants that hold unconditionally — every tile appears
exactly once, ratios sum to 1 and stay in (0, 1), `focused()` names a live
tile, `layout()` produces non-overlapping rects covering the bounds, and
`Session::from_toml(to_toml(ws)) == ws`. The example-based tests already encode
the healing rules (`crates/geode-shell/src/session.rs:1107`
`a_hostile_stack_node_is_healed_not_refused`, `:1350`
`from_toml_heals_a_dangling_focused_reference`, `:1270`
`from_toml_rejects_bad_ratios`, `:1309` `from_toml_rejects_a_nan_ratio`) —
a proptest would find the cases nobody thought to write, which is where the
memory notes say the real bugs lived ("a new projection stage touches four
Edit|Column matches").

**I3. Golden SQL tests for the query compiler.**
`crates/geode-data/src/query/compile.rs` has ~8 SQL assertions and they are
`contains`-shaped (`:2230-2231` asserts `ScopeSemantics::Direct` and
`predicate.contains("\"book\"")`; `:3421` asserts `sql(3) == sql(usize::MAX)`).
The silent-wrong-data contracts the docs emphasise most — per-measure grain
aggregation before the join, the as-of predicate's generation-ID list plus
exact `(batch, book, gen_id, source_time)` tuple match, NULL-book matchability
— are all *shapes of generated SQL*, and 34 mutation entries already mutate SQL
string fragments (m16). A golden file per (view, scope, grouping, as-of) case,
diffed on change, would make an unintended predicate change a visible diff
rather than a `contains` that still passes. The proptest at
`scope_sql.rs:1883` shows the crate already thinks this way.

**I4. A headless "every surface paints" smoke sweep.**
602 `debug_bounds` assertions against 175 `debug_selector` sites means the
selectors exist but coverage is uneven. One parameterised test that opens every
dialog and every tile kind and asserts each registered `debug_selector`
resolves to non-zero bounds would catch the "stage never painted" class — the
scopes-dialog handoff records that the final review "caught two Criticals tests
missed (stage never painted, query mirror)". Cheap, and it grows automatically
with each new selector.

**I5. Make the 8 ms budget measurable, then wire a CI floor.**
`docs/current/performance.md` concedes "CI compiles benchmarks but has no
stable regression baseline", and that is the right call for wall-clock on
shared runners. But a *coarse* floor (fail if a named bench exceeds 10× its
recorded median) catches the accidental O(n²), which is the regression class
that matters, without the noise sensitivity of a 5% threshold. Pair it with
M11's missing UI-action bench.

**I6. Share the object-dialog prologue and split the file.**
441 `run_until_parked()` and 155 `tempdir()` in one file, with 52 uses of
`dialog_test_shell_in_dir` and 39 of `open_tree_edit_stage`, says the factoring
is half done. Finish it (one `arrange` helper per stage that returns the shell,
the temp dir and the drawn context) and split per M15. This is the single
change that most reduces the cost of adding the next test.

**I7. A Latin-1 round-trip scan in CI.**
See M6. Twenty lines, no cargo, runs in the same job as I1's anchors gate, and
closes the one incident in the repo's history where the suite was green *because*
the expectations were wrong.

---

## (c) Systemic patterns

**The harness is the suite's conscience, and it is one layer short of
self-checking.** `scripts/mutation-check.sh`'s header (lines 1-110) is the best
piece of test documentation in the repo: it records that five rounds of review
found defects the suite could not see, that the fixture was the reason every
time, that a probabilistic-catch claim was measured and found wrong, and that
three separate concurrency/interrupt incidents drove the lock, the unique
backup path and the trap. Every entry names a test because a measured 79-entry
gap taught them to. The remaining gap is that the *filter* — the thing that
makes "caught" mean anything — is the one field nothing validates (C1, C2),
while the anchor is validated twice (per-run and in `--anchors-only`).

**Verification is strongest exactly where wrong data would be silent, and
thinnest where a human is assumed to be looking.** Sampling 21 documented
data-path contracts, 19 have named, well-titled tests — the as-of tie-break
fixture deliberately uses eight tied generations with the winner inserted first
(`crates/geode-data/src/query/as_of.rs:348-386`, with a comment explaining why
two was not enough); archive rows are asserted to keep their own `gen_id` *and*
`source_time` "not the successor's" (`store/publish.rs:391-435`); the health
lanes have `a_degraded_publish_survives_several_more_clean_discovery_polls`
(`service.rs:4027`), `a_clean_publish_of_one_batch_leaves_another_batchs_degraded_standing`
(`:4708`), `a_refused_health_event_is_offered_again_not_recorded_as_reported`
(`:4791`) and three startup-seed tests (`:4199`, `:4290`, `:4361`). Against
that: the 8 ms budget has no instrument (M11), ~24 visual contracts wait on a
human (m24), and the two uncovered data contracts (M14) are both
silent-failure-shaped.

**Window tests dominate the shell, and they mostly go through real input — but
the two seams at each end are dispatched, not driven.** 1,092 `#[gpui::test]`s
with ~1,300 key/pointer simulations is a serious investment, and the discipline
is visible: `dialog_test_shell_in` installs the production reclaimed keybindings
with a comment saying a `tab` press would otherwise prove nothing
(`tests/mod.rs:473-478`); `double_click` dispatches down/up at `click_count` 1
then 2 *with a draw between* because "the OS delivers them across frames"
(`tests/mod.rs:509-546`). But the *entry* seam is dispatched (m17: every dialog
test starts after the opening action fired) and one whole area is dispatched
throughout (M5/M4: `stacks.rs`, 19 direct updates), so the keymap→action edge
is proven by 3 integration tests that the mutation harness cannot reach (m21).

**Wall-clock leaks into tests of ordered state.** One real 20 ms sleep (M7),
seven sub-second negative waits (M8), and `now()`-derived stamps in the
diagnostics/flip/timeseries tests (M9). The project has `geode_core::clock::Clock`
and an `AppClock` global, and CLAUDE.md forbids `chrono::Local` for display for
exactly this reason — the same discipline has not reached the test clock.

**File size is now a verification risk in itself.** `objectdialog.rs` at 9,436
lines and 119 commits (M15), `marketdata/src/tile.rs` at 14,729 lines with 225
inline `#[gpui::test]`s starting at line 4,647, `pricer/src/tile.rs` at 5,236
with 99. The memory notes already record agents stalling on these, and C2's
corrupted filter traces directly to a helper name duplicated across two shell
test files.

**Documentation about verification is honest, which is rarer than good
coverage.** `docs/current/performance.md`'s "Known gaps" names eight specific
limitations including "A real painted frame is not covered by headless Criterion
benchmarks" and "CI compiles benchmarks but has no stable regression baseline";
`docs/current/data-path.md` closes with "Ordinary tests verify outcomes.
Targeted mutations … check whether tests can detect particular wrong-data
behaviors". `architecture.md:132-142` describes the four-layer strategy
accurately. The claims I could check were true; the gaps I found are ones the
docs either already name (the painted frame, the unscheduled retention sweep,
the diagnostics histogram collision) or simply have not noticed (the 8 ms
instrument, the two data contracts, the display-check backlog).

---

## (d) What is done well

1. **Test names state contracts.** Across 3,675 tests I found exactly one
   uninformative name (`crates/geode-shell/src/keymap/matcher.rs:178`
   `simple_match`) and zero `test_foo`-named tests. Names like
   `a_tie_on_source_time_resolves_to_the_newest_gen_id_every_time`,
   `a_degraded_load_survives_a_clean_discovery_poll`,
   `an_older_file_cannot_overwrite_the_bookless_partition`,
   `a_restored_tile_of_an_unknown_kind_paints_the_placeholder_and_its_record_survives`
   and `the_strip_should_paint_at_its_scaled_height` are the contract, readable
   in a failure line without opening the file. This is the single biggest
   quality signal in the suite.

2. **Fixtures are built to make the dangerous case reachable, and say so.**
   `as_of.rs:348-386` inserts eight tied generations in ascending ID order with
   a comment explaining that two made an unordered pick land wrong only half the
   time; the harness header records the measurement that drove it (4/4 caught
   after, 1/3 before). `publish.rs:391-435` asserts archived rows keep their own
   `gen_id` *and* their own `source_time`, "not the successor's".

3. **The mutation harness's operational engineering.** Unique per-run backup
   (`mktemp`), an atomic `mkdir` lock per checkout, and a trap on EXIT/INT/TERM
   that restores the in-flight file — each documented with the incident that
   caused it (`scripts/mutation-check.sh:30-46`), including one where a
   mutation was found committed to a working tree days later. Positional flag
   validation (`:172-179`) exists because a flag in the wrong slot once became
   the substring and silently checked nothing. The `caught*` verdict
   (`:294-302`) exists to surface "caught for the wrong reason". `--changed`
   computes its file set *before* the first mutation because the script edits
   tracked files (`:184-186`).

4. **Zero stale and zero ambiguous anchors across 1,575 entries, verified in
   0.185 s.** The gate works, it is genuinely sub-second, and the memory notes
   record it catching nine duplicates on introduction — one of which "was
   guarding the wrong site".

5. **Headless geometry testing is taken further than most projects manage.**
   `chrome_and_dialogs.rs:2670-2822` proves that the status bar, sidebar rail
   and command-line strip all scale with the rem *and* that the strip's painted
   bottom lands on the focused tile's bottom border — computing the expected
   tile rect through the real `dock_layout` and `Tree::layout`. Five mutation
   entries defend that one test. 602 `debug_bounds` assertions mean "is it
   painted, where, how big" is routinely asserted rather than eyeballed.

6. **The `test-support` feature is designed, not accreted.** Every cfg is
   `any(test, feature = "test-support")` (verified at
   `crates/geode-shell/src/{diagnostics.rs:535, module.rs:286,548,
   shell/mod.rs:1950,1962}`, `geode-core/src/{snapshot.rs:696,723,784,
   config/mod.rs:252}`, `geode-data/src/handle.rs:258`), so nothing is defined
   twice with both on. The self dev-dependency
   (`crates/geode-shell/Cargo.toml:61`) exists to stop cargo compiling 95k
   lines twice, with the reason in a comment; CI builds the feature explicitly
   because nothing else keeps that configuration compiling.

7. **Every bench target obeys the workspace invariant.** All 19 are
   `harness = false` with the owning lib at `bench = false`; verified across
   every `crates/*/Cargo.toml`. The requery bench really does run 1,000,000
   rows (`benches/query.rs:423`) with as-of variants, and the chart bench
   really does measure the 500k→1,600 cap the budget names
   (`benches/decimate.rs:57`).

8. **The UTF-8 incident produced the right *kind* of guard.**
   `crates/geode-blotter/src/delegate.rs:1198-1224` spells the glyphs as
   `\u{25B8}`/`\u{25BE}`/`\u{2020}` with the reasoning in the doc comment: "An
   escape cannot be re-encoded, so this test fails the moment the literals
   are." That is a mechanism, not a patch. It needs widening (M6), not
   rethinking.

9. **Comments record why a test is shaped the way it is.**
   `tests/mod.rs:607-616` explains that gpui's test executor never advances its
   simulated clock on `run_until_parked` — "confirmed against the pinned rev's
   `TestScheduler::run`, which is a plain `while step() {}` with no clock
   advancement" — so reload tests call `apply_reload` directly, and states that
   this is still the exact method the watcher calls.
   `tests/flip.rs:161` similarly names why real `Instant::now()` appears.
   A reader can tell a deliberate compromise from an oversight.

10. **Pure cores really are tested without a window, at scale.**
    `tiling/tree.rs` 101 tests, `tiling/workspaces.rs` 108, `session.rs` 56,
    `config_write.rs` 7 including atomicity, comment preservation, user-layer-only
    enforcement and a concurrent-edit ordering test. The layering
    `architecture.md:134-137` claims is real, not aspirational.

11. **`proptest` is used where round-trips are the contract.**
    `crates/geode-documents/src/cvi.rs:1453-1520` generates terms and nodes and
    asserts `parsed.rows == rows` with `unknown_paths` empty — the document
    family's parse/write invariant, which upload echo comparison depends on.

12. **No `#[ignore]`d tests anywhere in the workspace.** Nothing has been
    parked green.
