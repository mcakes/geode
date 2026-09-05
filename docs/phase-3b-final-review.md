# Phase 3b — whole-branch review (16ed397..ad5bf04)

Reviewed: the plan header / Global Constraints / "What already exists", every task's
Interfaces block, spec §2.1–§2.3, §3, §4, §6.8, the execution ledger, and the full diff
(31 files, +4647/−283) plus the working-tree files for `matcher.rs`, `context.rs`,
`groupings.rs`, `frame.rs`, `module.rs`, `commandline.rs`, `commandline_view.rs`,
`session.rs`, `defaults.rs`, `toolbar.rs`, `status.rs`, `whichkey.rs`, `perf.rs`,
`perf_overlay.rs`, `dialog.rs`, `sidebar.rs`, `main.rs`, `scripts/mutation-check.sh`,
`CLAUDE.md`, `docs/perf.md`, and the relevant ~1,900 lines of `shell/mod.rs`.

Verified locally (read-only; build output only): `cargo fmt --check` clean,
`cargo clippy --workspace --all-targets -- -D warnings` clean,
`cargo test -p geode-shell -p geode-core` green (711 lib tests + integration + doctests),
`cargo check -p geode-shell --features test-support` clean,
`grep -c '^run_mutation ' scripts/mutation-check.sh` = **134**.
Did not run `scripts/mutation-check.sh`, per instruction.

## Strengths

- **The count engine is vim-exact and defended rule by rule.** `matcher.rs:33-49`
  implements every clause of spec §3.3 — innermost-context-only, leading `0` stays a
  motion, a count survives `Pending` and dies on `NoMatch`, `escape`/`cancel` clear it,
  four-digit saturating cap — and there is one test *and* one mutation entry per rule
  (`digits_are_ordinary_keys_outside_a_counting_context`,
  `a_leading_zero_is_a_key_and_a_later_zero_is_a_digit`,
  `a_count_survives_a_pending_sequence_and_dies_with_a_dead_end`, `the_count_is_capped`).
  `a_digit_inside_a_pending_sequence_is_a_key_not_a_count` is the test I would have
  asked for and it is already there.
- **The `mod = "ctrl"` collision fix is the right shape and both halves are tested.**
  Moving the frame table *ahead* of the workspace table in `BUILTIN_KEYMAP` uses the
  matcher's own last-declaration-wins rule instead of adding a special case, and
  `defaults.rs`'s two new tests pin both aliases through the real
  `build_keymap` + `Matcher::press` path. No shipped binding moved — I diffed the
  workspace and context-less tables and they are byte-identical.
- **`ensure_occupants`' restored-kind guard** (`shell/mod.rs:1874-1877`,
  `let state = matched.and(restored.as_ref())`) is a genuinely subtle correctness fix,
  and it has both an isolating test (`a_restored_tile_of_an_unknown_kind_falls_back_without_its_state`)
  and a mutation entry that reverts exactly it.
- **The Task 3 ↔ Task 4 seam is tested end-to-end, not asserted at.**
  `current_tiles_reflects_live_occupants_and_restored_state_reaches_the_factory` builds a
  session table, runs it through the real `session::from_toml`, opens a real window, and
  asserts the factory received `Some(state)` — the exact composition a per-task review
  could not see.
- **The gpui tests verify painted behaviour and real focus**, not internal state:
  `cx.simulate_keystrokes` / `simulate_input` / `simulate_mouse_down`,
  `cx.debug_bounds("command-line" | "completion-row-N" | "frame-readout" | "tile-content-N" | "restart-required")`,
  and `focus_handle.is_focused(window)` after a click. The two click-to-focus tests
  (tree and dock) drive a real mouse-down onto a `RecordingView` that deliberately tracks
  its own `FocusHandle` — that is testing the hazard, not the happy path.
- **`take_dirty_session_write`'s split** (serialize on the UI thread, `write_atomic` on the
  background executor) plus the `last_tiles_written` comparison for state-only changes is
  a clean answer to "a module edit must persist without dirtying the layout flag on every
  keystroke", and `a_module_state_change_alone_flushes_once_with_the_new_state` isolates
  it from the already-covered layout trigger before asserting.
- **Layering held.** `geode-shell`'s dependency list is `geode-core`, `gpui`,
  `gpui-component`, `toml`, `serde_json`, `toml_edit`, `chrono` — no `geode-data`, no
  module crate; `geode-core::groupings` pulls in no gpui; the only data type crossing is
  `geode_core::query::QueryOutcome`. No raw colours in any new render code (grepped);
  the two file writes introduced (`persist_slot_to_user_config`, the session flush) both
  run on `cx.background_executor()`.
- **The per-render allocation ruling was honoured properly**: `scratch_all_tiles` /
  `scratch_active_tiles` with `fill_*` out-parameters, taken out of `self` and put back,
  so `ensure_occupants` allocates nothing once warm.
- **Doc comments are load-bearing and mostly accurate** — the strip's invariant comment,
  the `BUILTIN_KEYMAP` ordering comment, and `is_palette_toggle`'s explanation of why it
  resolves through the same last-exact-match-wins path the matcher uses (so a user
  rebinding `ctrl+k` is honoured even from inside the command line) are all correct and
  all things a future reader would otherwise have to rediscover.

## Issues

### Critical (Must Fix)

**C1. A second `tab` corrupts the command line. `crates/geode-shell/src/shell/mod.rs:1379-1397`**

`CommandLine::word` is the byte range the accept writes into. It is set only by
`CommandLine::refresh` (`commandline.rs:54-60`), which runs only from
`on_command_line_changed`, which runs only on `InputEvent::Change`. The Accept branch
calls `InputState::set_value`, and at the pinned rev `set_value` sets `emit_events = false`
around the replace (`gpui-component 0e2fb7a`,
`crates/base/src/input/base/state.rs:830-847`) — **no `Change` is emitted**, so `refresh`
never runs and `c.word` is never updated after an accept.

Trace, using the branch's own fixture (`completions = ["delta01", "gamma01"]`):

```
type ":sort a01"   -> word = 5..8, candidates = [delta01, gamma01]
tab   -> accept("sort a01", 5..8, "delta01") = "sort delta01"   (correct); word STILL 5..8
tab   -> accept("sort delta01", 5..8, "gamma01")
       = "sort " + "gamma01" + "sort delta01"[8..]
       = "sort gamma01ta01"                                      (corrupt)
```

Single-candidate case is just as bad: `:sort g` → tab → `sort gamma01` → tab →
`sort gamma01amma01`. Spec §3.4 mandates this interaction verbatim ("`tab` accepts the
top row and cycles on repeat"), so this is the primary documented path, not an edge.
The existing test presses `tab` exactly once, which is why it is green.

Why it slipped: the ledger's Task 5 deferred minor records the `mem::take`/restore dance
around `set_value` as "inert (brief-verbatim)". That call is correct — the dance is inert
*because* `set_value` emits nothing — but the same fact is what makes `c.word` stale, and
the minor was filed rather than chased.

Fix: `accept` already returns the new cursor, which is exactly `word.start + candidate.len()`:

```rust
let (line, cursor) = commandline::accept(&text, c.word.clone(), &word);
c.word = c.word.start..cursor;
```

and delete the now-provably-inert `mem::take` dance. Add a test that presses `tab` twice
and asserts the input value, and a mutation entry anchored on the new `c.word = …` line
naming that test.

### Important (Should Fix)

**I1. Any surface that steals focus without going through `cancel_command_line` orphans the line. `crates/geode-shell/src/shell/sidebar.rs:84`, `crates/geode-shell/src/shell/mod.rs:2151-2181`**

The fix-round-1 ruling established the invariant "the focused tile IS `command_line.tile`
while the line is open", on the reasoning that keyboard input cannot move focus (the
command-line branch precedes the matcher) and both tile mouse-down handlers cancel. Two
mouse paths were missed:

1. **Sidebar workspace switch.** `sidebar.rs:84` dispatches `workspace::switch_N` on
   mouse-down. It does not cancel the line. The render comment at `shell/mod.rs:3585-3605`
   then paints the strip at the *new* workspace's `focused_rect`, while
   `handle_command_line_key` still routes `enter` to `self.occupants.get(&line.tile)` —
   a tile in the workspace you just left. `:sort delta01` runs against an off-screen tile
   with the strip sitting over an unrelated one. The invariant the render comment asserts
   is false as written.
2. **Toolbar filter click.** Clicking `filter_input` focuses it; the command-line branch
   is then skipped (it requires `command_input` focused), the filter branch returns, and
   the strip stays painted, stale, and deaf. `escape` in the filter returns focus to the
   shell root without closing it, so the next `/` replaces a still-open line.

Fix: rather than adding a third and fourth `cancel_command_line` call site, close the
class — `cx.on_focus_out` on `command_input`'s handle (or a check at the top of `render`:
`command_line.is_some() && !command_input.focused` ⇒ `cancel_command_line`) — and then
correct the render comment at `shell/mod.rs:3588-3599`, which currently claims mouse-down
on a tile is the only way focus moves.

**I2. An occupant created outside the active workspace is never told it is hidden. `crates/geode-shell/src/shell/mod.rs:1846-1897`**

`fill_all_tiles` walks every workspace and every dock, so `ensure_occupants` creates an
occupant for all tiles across all 9 workspaces on the first render. The visibility diff
then runs only against `fill_active_tiles` (active workspace, visible docks) with
`visible_tiles` starting empty:

- a new tile in the *active* set → `set_visible(true)` (correct);
- a new tile in an *inactive* workspace or a *collapsed dock* → **nothing, ever**. It is
  never in `visible_tiles`, so `visible_tiles.difference(&active)` never yields it either.

Neither `TileContent::set_visible` (`module.rs:47-48`) nor `ModuleFactory::create` states
what an occupant's visibility is at construction. Spec §3.1 says "Hidden tiles … may drop
subscriptions; shown tiles resubscribe and requery if stale", which reads as "the shell
tells you". In Plan 3c a blotter that defaults to visible will hold live subscriptions and
requery for up to eight off-screen workspaces for the life of the process, with no signal
that anything is wrong — exactly the silent class of defect the mutation harness exists for.

Fix: compute `active` before the creation loop and call
`occupant.content.set_visible(active.contains(id), cx)` on each newly created occupant
(then the diff loop below is unchanged), or — minimally — document "an occupant is created
hidden; the shell announces visibility on the first render after creation" on both trait
methods. Add a test: two workspaces, assert the inactive workspace's occupant recorded
`Visible(tile, false)` (or was never told `true`).

**I3. `CLAUDE.md` inverts the frame's central design property. `CLAUDE.md:14`**

> "…the shell now hosts module occupants through the hosting contract (`geode_shell::module`),
> with **each tile holding a `Frame` entity**…"

There is exactly one `Frame` entity, on `ShellView` (`shell/mod.rs:877`), whose handle is
cloned into every `ModuleFactory::create` call (`shell/mod.rs:1885`). Spec §4.1's whole
mechanism — "one uniform frame-subscription mechanism … one integer compare per counter per
notification" — depends on it being shared; per-tile frames would make `ctrl+1` a per-tile
action. The ledger flagged this and left it for this review. Fix: "…with one shared `Frame`
entity every tile observes…".

Two smaller doc points in the same paragraph, worth folding into the same edit: the Phase 1
sentence was amended to list count prefixes, module hosting, the frame and the command line
as part of "Phase 1 (the shell) is complete", which reads as though they shipped in Phase 1;
and "slot numbering via `ctrl+0..9`" should note that under `keymap.mod = "ctrl"` the
shipped `workspace::switch_N` bindings win `ctrl+1..9` and the slots are palette-only.

### Minor (Nice to Have)

- **M1. `Frame::readout()` allocates every frame.** `frame.rs:214-242` builds a
  `Vec<String>`, up to four `format!`s and a `join` on every call; `shell/mod.rs:3285`
  calls it unconditionally at the top of every `render`, changed or not. PHILOSOPHY's
  "per-frame heap churn is a defect" is stretched here beyond the plan's sanctioned
  small-String class. Cache a `FrameReadout` keyed on `Frame::versions()`.
- **M2. `GroupingSlots::from_doc` silently drops non-string elements.** `groupings.rs:51-56`
  uses `filter_map(|v| v.as_str())`, so `1 = ["book", 3]` yields `["book"]` with no
  diagnostic — a slot quietly grouping by one fewer column than the user wrote. Emit a
  warning (or drop the slot) when an element is not a string.
- **M3. `Frame::undo_scope`'s `previous == self.scope` guard is unreachable.**
  `frame.rs:106-108`: `set_scope` only records `previous_scope` when the value differs, and
  nothing else writes it. Dead branch; delete it. (ledger deferred)
- **M4. `word_at` can panic on a non-char-boundary cursor.** `commandline.rs:89-93` clamps
  with `min(line.len())` but then slices `line[..cursor]`. `InputState::cursor` should always
  be on a boundary, but the pure core should not depend on that — clamp down to the nearest
  `is_char_boundary`.
- **M5. The completion popup does not follow the highlight past row 8.**
  `commandline_view.rs:17,59,70` caps at `MAX_ROWS = 8` while `CommandLine::step` cycles
  over all candidates, so `ctrl+n` nine times highlights an invisible row.
- **M6. `chrono` is a runtime dependency but used only in tests.** `geode-shell/Cargo.toml:34`
  — the only `chrono::` paths in the crate are in `frame.rs`'s `#[cfg(test)]` module;
  `AsOf::At(t).format(..)` is an inherent method reached through `geode-core`'s public type.
  Move it to `[dev-dependencies]`.
- **M7. Readout element order deviates from spec §4.4**, which says "left to right: the
  active slot …, the scope …, and, when as-of is set, `AS OF 14:05`". `toolbar.rs:47-73`
  paints AS OF first. Defensible (it is the unmissable element) but undocumented; either
  reorder or note the deliberate change.
- **M8. `restart_required` is never cleared** (`shell/mod.rs:573,1052`). Reverting the
  offending `sources.toml` edit leaves "sources changed — restart to apply" up for the rest
  of the session.
- **M9. `theme::write_atomic`'s temp file is hardcoded `.app.toml.{pid}-{n}.tmp`**
  (`theme.rs:529`) and is now also used to write `groupings.toml`. Safe (pid+counter make it
  unique) but the name lies; parameterise it or rename to `.geode-write.{pid}-{n}.tmp`.
  (ledger deferred)
- **M10. The which-key count row is only reachable with a sequence also pending.**
  `whichkey::render` is gated on `!matcher.pending().is_empty()` (`shell/mod.rs:3306`), and a
  bare count leaves `pending` empty — so `4` shows in the status bar but never in the
  overlay. Consistent with the overlay's "nothing to say with no sequence in flight"
  contract, but spec §3.3 asks for both; add a comment saying which reading was chosen.
- **M11. Render-level assertions are thin for the three new readouts.**
  `ctrl_digits_switch_the_frame_slot_and_ctrl_0_clears_it` asserts only that
  `frame-readout` painted, not its content; the status-bar count and the which-key count
  have no render-level test at all (ledger's Task 1 deferred minor). A wrong label or a
  missing count would not fail anything.
- **M12. `module.rs:362` `registering_actions_delegates_to_every_factory_once` does not test
  what its name and comment claim** — the "registering twice" half uses a *fresh*
  `ActionRegistry`, so nothing is ever registered twice into the same registry.
  (ledger deferred)
- **M13. `take_dirty_session_write` calls `serialize` on every occupant in every workspace,
  on the UI thread, every 500ms, unconditionally** (`shell/mod.rs:1740-1745`, driven from
  `shell/mod.rs:732`), plus a `TileRecords` allocation per tick. Fine at 3b's scale;
  revisit in 3c if a blotter's `serialize` grows (gate on a cheap per-occupant epoch).
- **M14. `test-support` is not exercised by CI.** `cargo clippy --all-targets` does not
  enable it. It builds today (I checked), but nothing keeps it building until 3c depends on
  it. Add `--all-features` to one CI step, or a `cargo check -p geode-shell --features test-support`.
- **M15. `current_tiles` drops a `placeholder` occupant's record** (`shell/mod.rs:1708`), and
  a restored record whose kind is unknown is consumed by `ensure_occupants` and rewritten as
  the *default* kind with empty state. Net effect: a misconfigured or downgraded run silently
  discards a tile's saved module state on the next flush. Acceptable healing, but worth a
  sentence in `session.rs`'s docs so it is a decision rather than a surprise.
- **M16. Spec §3.3 says dispatch tries "the shell's own arms first, then
  `apply_workspace_action`, then … the occupant"; `dispatch` does workspace first**
  (`shell/mod.rs:1452`). Pre-existing structure, equivalent while no ids collide — but the
  spec sentence should be corrected rather than left as a latent contradiction.

## Deferred-minor triage

| Ledger line | Verdict |
|---|---|
| T1 — no render-level test of the count in `status.rs`/`whichkey.rs` | defer to 3c (M11) |
| T2 — `undo_scope`'s unreachable `previous == self.scope` guard | defer to 3c (M3) |
| T2 — `theme::write_atomic` temp name hardcoded `.app.toml.*` | defer to 3c (M9) |
| T2 — no mutation entries for `save_slot`'s conditional grouping bump / `undo_scope` one-level | defer to 3c (tests exist; add entries with the 3c batch) |
| T3 — `registering_actions_delegates_to_every_factory_once` asserts nothing; comment misleading | defer to 3c (M12) |
| T3 — no harness entry for `set_visible` being diffed rather than broadcast | defer to 3c — but fold in the I2 test when that lands, since it is the same code path |
| T4 — `from_toml` tiles drop branches lack their own mutation entries | defer to 3c (all four branches do have tests; only the entries are missing) |
| T4 — `shell/mod.rs` restructuring pass wanted | defer to 3c — but it is now 11,393 lines and 3c adds more; schedule it as the first 3c task, not the last |
| T5 — `commandline_view` render comment implies `focused_rect` is always the line's tile | **fix before merge** — with I1 the comment is not merely imprecise, it is false; fix it in the same commit |
| T5 — inert `mem::take`/restore around `set_value` in the Accept branch | **fix before merge** — it is the same three lines as C1; the dance goes when `c.word` is fixed |
| T5 — dock-tile `cancel_command_line` has no isolating test or mutation entry | defer to 3c (behaviour is present and shared with the tree path, which is covered) |
| T5 — `filter_input` has the same pre-existing modal/palette focus gap | defer to 3c *as stated* (pre-existing), but the **converse** — a filter click orphaning the command line — is new and is part of I1 |
| T6 — slot-rebuild block duplicated between `new` and `apply_reload` | defer to 3c |
| T6 — `Frame::save_slot` clones the grouping before validating | defer to 3c |
| T6 — datasets-alone reload (restart + slots recomputed) not asserted | defer to 3c |
| T7 — `perf::reset` → `frame.requery` has no integration assertion; no entry for the pairing logic | defer to 3c (no call sites exist until then; the pure `RequeryStats` test covers pairing) |
| T8 — "each tile holding a `Frame` entity" | **fix before merge** (I3) |

## Rulings, judged on their merits

Every controller ruling in the ledger is consistent with the spec; none needs reversing.

- **T2 `undo_scope` `take()`-based one level** — correct. Spec §4.3: "Restore the remembered
  scope. One level." The brief's swap-based snippet would have ping-ponged forever.
- **T3 (a) dock click re-arms `pending_focus_restore`, (b) scratch sets over per-render
  `HashSet`s, (c) fallback factory must not see a mismatched record's state** — all three
  correct, all three tested; (b) correctly ranked the Global Constraint above the brief's
  verbatim snippet.
- **T5 (1) mirror `close_palette`, (2) cancel on any tile mouse-down** — correct in
  direction; (2)'s invariant is *incomplete*, not wrong (see I1).
- **T6 (1) keep spec `ctrl+1..9` but declare the frame table first** — sound, and the better
  of the two options: it keeps a shipped promise (`mod+N` workspace switching) intact and
  leaves the slots keyboard-reachable through the palette, which satisfies PHILOSOPHY's
  "every action must be keyboard-reachable". Note the asymmetry it creates: under
  `mod = "ctrl"`, `ctrl+0` (`frame::slot_clear`) still works because no `mod+0` binding
  exists, so a user can clear a slot by key but not set one. Worth one line in the
  `BUILTIN_KEYMAP` comment.
- **T6 (2) comment fix, (3) two more reload entries** — correct.
- **Pre-flight ruling that Task 2's step text (not its Files list) governs `perf.rs`** — correct.

## Plan and spec issues

- **The plan's Task 5 Interfaces block never says who maintains `CommandLine::word` after an
  accept.** It specifies `word_at`, `accept` and the `Ranked` bookkeeping, but the accept
  path's own post-condition is unstated — and that gap is precisely where C1 lives. A future
  plan writing a "pure state + shell mutates it" pair should state the invariant on each
  mutable field.
- **Spec §4.2 mandates `ctrl+1..9` without noticing the `keymap.mod = "ctrl"` alias**, which
  `defaults::mod_alias_from_config` supports and which makes `mod+1` and `ctrl+1` the same
  keystroke. The T6 ruling resolves it; the spec should record the resolution rather than
  leave the next reader to rediscover the collision.
- **Spec §3.3's dispatch order sentence does not match the implementation** (M16).
- **Spec §4.5 asks for `ShellEvent::ConfigReloaded` on "views, dimensions and groupings";**
  the implementation emits it for views/dimensions only, since the event's sole consumer is
  the data thread's `ReplaceViews` and groupings are shell-side. That is the right call — but
  it is a deviation the spec should absorb, not just a code comment.

## Mutation-harness spot check (three entries, three tasks)

- **T1 `matcher: the count is capped` → `the_count_is_capped`.** The mutation deletes
  `.min(MAX_COUNT)`, leaving a valid trailing comma; eight `9`s then accumulate to
  99,999,999 and the test's `assert_eq!(m.count(), Some(MAX_COUNT))` fails. **Honest.**
- **T3 `hosting: an unknown action reaches the focused occupant with its count` →
  `a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count`.** The mutation passes
  `None`; the test matches `Recorded::Dispatch(t, a, Some(4))` after `simulate_keystrokes("4 j")`
  through the real matcher. **Honest, and it is the count that fails, not a marker.**
- **T6 `frame: a views/dimensions change reaches the frame and emits ConfigReloaded` →
  `a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart`.**
  The test subscribes to `ShellEvent` and asserts `contains(&ConfigReloaded)`, so disabling
  the branch fails it. **Honest** — though note the co-located `versions.config > v0.config`
  assertion in that test is *not* isolating (`replace_slots` also bumps `config`); the event
  assertion is what carries the entry.

One structural note: `run_mutation` applies `str.replace(from, to, 1)` — first occurrence
only. `commandline: a tile mouse-down cancels an open line` therefore mutates the tree-tile
listener only; the dock listener's identical call is undefended, which matches the ledger's
own deferred minor.

## Production readiness

- **Older `session.toml` (no `tiles`)** — `parse_workspace` guards on
  `ws_table.get("tiles")`, so a pre-3b file loads with an empty `TileRecords` and every tile
  gets a default-kind occupant. Safe. `config_version` is unchanged at 1, correctly: the
  addition is purely additive.
- **`groupings.toml` absent** — `Config::doc("groupings")` returns `None`,
  `unwrap_or_default()` yields empty slots, `set_active_slot(Some(n))` refuses an empty slot
  and bumps nothing. `ctrl+1..9` are inert no-ops, not errors. Correct.
- **`groupings.toml` malformed** — a parse error becomes an error-severity `Diagnostic`;
  `reload::decide` keeps the last-good `Config` whole. Bad keys, non-array values, empty
  arrays and unknown columns are per-slot warnings/errors that drop only that slot
  (`bad_keys_and_shapes_warn_and_are_ignored`). Only M2's mixed-type array is silent.
- **Windows** — no macOS-only API introduced; all paths are `PathBuf::join`; the new atomic
  write reuses the existing pid+counter temp-name scheme (which exists precisely because of
  Windows rename semantics). `":"` and `"/"` are bound as the resolved symbols, matching the
  `BUILTIN_KEYMAP` doc comment's shifted-symbol rule. Nothing here should differ across the
  two CI platforms.
- **`bench = false`** — no new lib/bin/bench targets; `geode-shell`'s existing `bench = false`
  and `[[bench]] harness = false` are untouched, and `benches/shell_cores.rs` was updated for
  the new `to_toml`/`from_toml` signatures.
- **`save_slot` vs `apply_reload` race** — checked and benign. `Frame::save_slot` updates
  memory and stashes `pending_persist`; `on_frame_changed` drains it and writes on the
  background executor. A reload landing in that window only calls `replace_slots` when
  `groupings.toml` itself differs between the in-memory `Config` and the freshly loaded one
  — both still hold the pre-write content, so `changed("groupings")` is false and nothing is
  reverted. Once the write lands, the mtime change triggers a reload whose `replace_slots`
  is a no-op (`self.slots == slots` ⇒ `false`, no notify), so there is no feedback loop
  either.

## Recommendations

1. Fix C1 (`c.word` after accept) with a two-tab test and a mutation entry; delete the
   `mem::take` dance in the same change.
2. Fix I1 by closing the class — a focus-out handler on `command_input` — rather than adding
   two more `cancel_command_line` call sites, and correct the render comment that asserts the
   now-false invariant.
3. Fix I2 by announcing initial visibility at creation, and state the contract on
   `TileContent::set_visible` / `ModuleFactory::create` so Plan 3c cannot get it wrong.
4. Fix I3's one sentence in `CLAUDE.md` (plus the two smaller points in the same paragraph).
5. Re-run the unfiltered harness after those four land — C1's and I2's fixes touch
   `shell/mod.rs` behaviours that existing entries anchor on.
6. Schedule the `shell/mod.rs` split as the **first** task of Plan 3c. At 11,393 lines with
   ~7,600 of them tests in one `mod tests`, the file has passed the point where a reviewer can
   hold it; C1 survived four reviews partly because the Accept branch is 1,400 lines from the
   pure core it depends on.

## Assessment

**Ready to merge?** With fixes

**Reasoning:** The architecture, layering, key routing and test discipline are strong, and
every cross-task seam I probed except one is correct and directly tested — but a second `tab`
on the command line writes a corrupted line on the spec's own primary completion interaction
(C1), and three Important gaps (an orphanable command line, occupants never told they start
hidden, and a `CLAUDE.md` sentence that inverts the frame's central design property) would
each cost more to discover in Plan 3c than to fix now.
