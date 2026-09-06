# Phase 3c handoff — start here in a fresh session

**Phase 3c landed** — the blotter, `--demo`, and the probe deletion are
done. See `CLAUDE.md`'s Phase 3 paragraphs for the current state; this
document is kept as the historical kickoff note.

Written 2026-09-05 at the end of the Phase 3b session. Everything a new
session needs to execute Plan 3c, with the `shell/mod.rs` split done first.

## Prompt to paste into the new session

> Read `docs/phase-3c-handoff.md` and follow it. Execute Plan 3c
> (`docs/superpowers/plans/2026-09-03-phase-3c-blotter.md`) subagent-driven,
> with one change the handoff explains: split `crates/geode-shell/src/shell/mod.rs`
> as a new first task before the blotter work. Brainstorm the split boundaries
> with me briefly, amend the plan, then run it. Standing merge authorization
> applies (see memory `working-rhythm`); ask before anything else that leaves the
> worktree.

## Where things stand

- `main` is at `0acd499` (Phase 3b merged). Phases 1, 2, 3a and 3b are done;
  `CLAUDE.md` describes each. `docs/modules.md` is Matthew's untracked draft:
  leave it alone.
- Plan 3c has nine tasks: crate skeleton and `ColumnPlan`; expansion, flatten and
  the depth bound; cursor, modes, find; number formatting, format cache, yank; the
  `:` grammar and completions; `BlotterDelegate` (the `DataTable` adapter);
  `BlotterTile`/`BlotterContent`/`BlotterFactory`; the `geode-app` data bridge,
  roster, database path and `--demo`; probe deletion, benches, docs, harness.
- Spec: `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
  (§5 data path, §6 blotter, §7 demo and tests). It is the binding authority
  where a plan disagrees with it.

## The plan change: split `shell/mod.rs` first

`crates/geode-shell/src/shell/mod.rs` is about 11,400 lines, roughly 7,600 of
them one `mod tests`. The Phase 3b final review found that its one Critical
defect survived four reviews partly because the command line's accept branch sat
1,400 lines from the pure core it depended on. Plan 3c adds more to this file
(occupant routing, `deliver`, the bridge), so the split goes first, not last.

Brainstorm the boundaries with Matthew before writing the task. A starting
proposal, to be confirmed against the file:

| New file | Moves out of `mod.rs` |
|---|---|
| `shell/keys.rs` | `handle_key_down`, `is_palette_toggle`, the command-line key branch, `context_stack` |
| `shell/occupants.rs` | `ensure_occupants`, `fill_all_tiles`/`fill_active_tiles`, `current_tiles`, `deliver`, `occupant_kind` |
| `shell/commandline_ctl.rs` | `open_command_line`, `close_command_line`, `cancel_command_line`, `handle_command_line_key`, `on_command_line_changed`, the Accept branch |
| `shell/reload.rs` (or extend the existing `reload` module) | `apply_reload`, the slot rebuild shared with `new` (a deferred DRY finding) |
| `shell/session_io.rs` | `take_dirty_session_write`, `save_session`, `last_tiles_written` |
| `shell/tests/*.rs` | the `mod tests` block, split by the same seams |

Rules for the split task: pure moves, no behaviour change; `cargo test` count
unchanged (1070 workspace, 715 in `geode-shell` lib); every mutation-harness
anchor re-verified with `zsh scripts/mutation-check.sh --changed` (moving code
detaches anchors silently, which happened in Phase 3a Task 9); `pub(super)` over
`pub` for anything that only the shell needs. Review it as its own gate before
Task 1 of the blotter starts.

## Findings deferred from 3b into 3c

From the final review (`docs/phase-3b-final-review.md`;
the M-numbers are its). Fold each into the 3c task that touches the area, or
into the split task when it is a pure cleanup:

- M1 `Frame::readout()` allocates every frame; cache a `FrameReadout` keyed on
  `Frame::versions()`.
- M2 `GroupingSlots::from_doc` silently drops non-string array elements; warn or
  drop the slot.
- M3 `Frame::undo_scope` has an unreachable `previous == self.scope` guard.
- M4 `commandline::word_at` can panic on a non-char-boundary cursor; clamp to a
  boundary.
- M5 the completion popup caps at 8 rows while `step` cycles all candidates.
- M6 `chrono` is a runtime dependency of `geode-shell` but used only in tests.
- M7 the readout paints AS OF first; spec §4.4 lists it last. Reorder or record
  the deliberate change.
- M8 `restart_required` is never cleared once set.
- M9 `theme::write_atomic`'s temp-file name is hardcoded `.app.toml.*` and now
  also writes `groupings.toml`.
- M10 the which-key count row only shows with a sequence pending; add a comment
  saying which reading of spec §3.3 was chosen.
- M11 render-level assertions are thin for the readout, status-bar count and
  which-key count.
- M12 `module.rs` `registering_actions_delegates_to_every_factory_once` never
  registers twice into the same registry.
- M13 `take_dirty_session_write` serializes every occupant every 500 ms on the UI
  thread; gate on a cheap per-occupant epoch once the blotter's `serialize`
  grows.
- M14 the `test-support` feature is not exercised by CI; add
  `cargo check -p geode-shell --features test-support` (3c's blotter tests will
  depend on it).
- M15 a restored record of unknown kind is rewritten as the default kind with
  empty state on the next flush; document it in `session.rs`.
- M16 spec §3.3's dispatch-order sentence does not match the code.

Smaller ledger items: mutation entries missing for `save_slot`'s conditional
grouping bump, `undo_scope` one-level, the four `from_toml` tile-drop branches,
the dock-tile `cancel_command_line` call, and `RequeryStats` pairing; the
slot-rebuild block duplicated between `ShellView::new` and `apply_reload`;
`Frame::save_slot` clones before validating; `filter_input` has the same
pre-existing modal/palette focus gap the command line fix closed.

Deferred from the 3a ledger: `worst_health` tie-break; `Health.source` should
carry a dataset name; config-honesty nits in `sources.toml`; the NULL-depth
phantom-root comment in `TreeIndex`; `children`/`children_of` naming; a one-step
FNV mix of the u64 dictionary code.

Spec corrections to make while in there: §3.3 dispatch order (workspace arms run
before the shell's own); §4.2 should record that under `keymap.mod = "ctrl"` the
shipped `workspace::switch_N` bindings win `ctrl+1..9` and slots are set via the
palette; §4.5 should say `ShellEvent::ConfigReloaded` fires for views and
dimensions only, since groupings are shell-side.

## Constraints carried from the original Phase 3 request

- `[sources]` landed in 3a; the probe is deleted only in 3c Task 9, after the
  painted-frame test story for the §7.1 budget exists (spec §7).
- The blotter must not do per-cell `Schema::index_of` name lookups: resolve
  column indices once per snapshot (`Snapshot::column_index`, then the `*_at`
  accessors in `geode-core/src/snapshot.rs`).
- Honour the read path's opinions: a NonAttributable cell is NULL, so blank is
  never `0.00`; depth leads ORDER BY, so a flatten may assume parent before
  children; attribution markers are per row.
- Plan 3c Task 8 (the bridge): the `EventSink` channel is bounded and fed with
  `try_send` only; count refused events as `dropped_events` and surface them as a
  diagnostic; shut down the last `DataHandle` on the background executor, never
  on the UI thread.

## How the 3b session ran, so 3c can copy what worked

- `superpowers:subagent-driven-development`: `EnterWorktree` for isolation, a
  ledger under `.superpowers/sdd/<plan>/progress.md`, fresh implementer per task,
  one task reviewer, scoped re-reviews of fix rounds, one final whole-branch
  review, one fix wave, merge. Every ruling goes in the ledger with its cost if
  wrong and is repeated to Matthew at the end.
- Models: sonnet implementers and reviewers throughout; haiku for tiny mechanical
  tasks and small re-reviews; opus only for the final whole-branch review. Opus
  implementers stalled repeatedly in earlier sessions. Sonnet stalled once at
  ~290k tokens (Task 3); a fresh implementer with the brief, report and findings
  recovers cleanly.
- Every subagent command in the foreground with a timeout under 600000 ms; a
  backgrounded command from a subagent stalls it. The worktree guard refuses
  compound commands that name `git` or `.git` paths; keep them plain.
- Harness: `zsh scripts/mutation-check.sh --changed` after every task; every new
  entry gets a 6th-argument test filter; commit before mutating; the unfiltered
  run (137 entries, 55 minutes) once at the end, started detached by the
  controller after the final review so no reviewer reads a mutated file. Match
  its process with `pgrep -f "^zsh scripts/mutation-check.sh"`, never a bare
  `pgrep -f`.
- Merge: `ExitWorktree(keep)`, check `main` for hand-edits (only `docs/modules.md`
  untracked is expected), `git merge --no-ff`, run the workspace tests on the
  result, remove the worktree and branch.
