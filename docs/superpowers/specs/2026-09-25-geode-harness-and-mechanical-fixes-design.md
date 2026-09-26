# Geode — Mutation Harness Linting and Mechanical Fixes Design

The first work unit arising from the 2026-09-25 codebase review
(`review-2026-09-25/`). That review produced findings across fifteen areas;
this document specifies only the harness repair and the mechanical fixes,
which were chosen to go first because the harness is the instrument that
verifies everything after it.

Amends nothing. Adds checks to `scripts/mutation-check.sh` and fixes four
defects the review verified.

## 1. Why this changes

`scripts/mutation-check.sh` validates each entry's anchor twice: once in
`--anchors-only` (the merge gate) and once in a normal run, reporting
`ANCHOR` for a stale anchor and `AMBIG` for a duplicated one. It never
validates the *test filter*, which is the field that makes a `caught`
verdict mean anything. The header of the script already names the hazard
this leaves open: without a filter, "`caught` says nothing about WHICH
test saw the mutation, which is the 'two defences overlapping' lie".

One entry names a test that does not exist:

```
run_mutation "pricer tile: a commit ignores that its line went away" \
  crates/geode-pricer/src/tile.rs \
  … \
  geode-pricer a_commit_whose_line_went_away_is_refused
```

No such function exists; the nearest is
`an_editor_whose_line_went_away_closes_with_moved`
(`crates/geode-pricer/src/tile.rs:3825`). The script prints
`FILTER … matches no test`, clears the filter, runs all 201 `geode-pricer`
tests, and reports `caught` on the strength of an unrelated test. That
warning is invisible to the merge gate, because `--anchors-only` returns
before any filter is considered, and invisible to `--changed` unless
`geode-pricer/src/tile.rs` is in the changed set.

Three further entries, across two filters, name no test either and match
only by accident. Measured against the real function names in each
package:

| Filter matches | Entries |
|---|---:|
| Exactly one test | 1,564 |
| Filter equals one of several matched names | 7 |
| Several matches, none equal to the filter | 3 |
| No match at all | 1 |

Alongside the harness, three code defects and two documentation and CI
gaps are fixed here because each is mechanical, independently verifiable,
and needs no ruling.

## 2. Why not `--exact`

The obvious fix is to pass libtest's `--exact`. It is wrong, measurably.
`--exact` matches the *full test path*, and every one of the 1,575 filters
is a bare function name:

```
$ cargo test -p geode-chart --lib -- --exact ticks_respect_the_gap
running 0 tests … 51 filtered out

$ cargo test -p geode-chart --lib -- ticks_respect_the_gap
test result: ok. 1 passed … 50 filtered out
```

Adopting it would make every entry match nothing, fall through to the
full-suite branch, and turn the measured ~3 s filtered entry into ~36 s.
The cost note in the script's header exists because that difference took a
`--changed` run over `service.rs` and `catalog.rs` past an hour.

The check therefore belongs in the linter, which must mirror cargo's
matching exactly: substring, against function names.

## 3. The linter

Four checks are added to the `--anchors-only` pass. That pass already
reads every anchored file and runs no cargo; collecting test-function
names per package adds one read per source file and keeps the whole pass
well under a second.

**Filter checks.** A filter passes when it matches exactly one
test-attributed function in its package, **or** when one of several
matched names equals the filter exactly. The second clause is load-bearing:
six of the seven multi-match filters are *prefixes* of their siblings, so
no substring can exclude them. `patch_cell_matches_a_rebuild` is a prefix
of both `patch_cell_matches_a_rebuild_under_a_pivot` and
`patch_cell_matches_a_rebuild_with_rows_spliced`. Without the clause, the
only remedy would be renaming tests, and in this repo a test name is a
statement of a contract.

- `FILTER  <name>  <-- '<filter>' matches no test` — hard failure.
- `FILTERx N  <name>  <-- '<filter>' matches N tests, none of them exactly`
  — hard failure. The named test never runs, so the verdict comes from
  whatever else the substring caught.
- `FILTER? N  <name>  <-- '<filter>' also matches N-1 sibling tests` —
  warning. The named test does run, so the verdict is sound; the siblings
  make it slower and make "which test caught it" unanswerable.

**Anchor-shadowing checks.** Both are warnings in this branch and are
promoted to hard failures in the follow-up that fixes them, which is a
one-line change.

- `DUP  <name>  <-- shares (file, anchor) with <other>` — two entries
  carry an identical file and anchor. `replace(…, 1)` mutates the first
  occurrence, so the second entry defends nothing.
- `SHADOW  <name>  <-- anchor is a substring of <other>'s in the same file`
  — the existing `AMBIG` check cannot see these, because the short anchor
  legitimately occurs once in the file while sitting inside the longer
  construct the other entry meant.

Measured on this checkout, the shadowing checks will report 45 duplicate
groups covering 93 entries, and 43 shadowed anchors.

The summary line gains the new counts:

```
checked 1575 anchors: 0 stale, 0 ambiguous, 0 bad filters
  7 loose filters, 93 duplicate anchors in 45 groups, 43 shadowed anchors
  (warnings; see the anchor-uniqueness follow-up)
```

Exit is non-zero on any `ANCHOR`, `AMBIG`, `FILTER` or `FILTERx`, and zero
on `FILTER?`, `DUP` and `SHADOW` alone, so this branch leaves CI green.

**Scope of matching.** The linter compares against function names, not
full module paths. Measured, this produces zero false positives: all 1,575
filters are snake-case behaviour sentences, and building module paths would
add parsing for no present gain. A future filter that matched only through
a module name would be reported as `FILTER`, and the fix would be to name
the test.

## 4. The four broken filters

Each is repaired by naming the test the entry meant. All four resolutions
were determined by reading the mutation against the candidate tests, so the
implementation does not need to guess.

**`pricer tile: a commit ignores that its line went away`** filters on
`a_commit_whose_line_went_away_is_refused`, which does not exist. The
mutation makes `Sheet::index_of` fall back to row 0 for a line that is
gone. Name `an_editor_whose_line_went_away_closes_with_moved`
(`crates/geode-pricer/src/tile.rs:3825`), then confirm by probe that it
fails under the mutation; if it does not, the entry is guarding a contract
no test states and that is itself the finding.

**The `geode-shell` reload entry** filters on
`emits_config_reloaded_before_the_frame_notifies`, which names no test and
matches two. Its mutation removes the `cx.emit(ShellEvent::ConfigReloaded)`
inside `if views_changed`, so the views path is the one it defends. Name
`a_views_change_emits_config_reloaded_before_the_frame_notifies_its_observers`.

**Both `geode-marketdata` key-switch entries** filter on
`outcome_after_a_key_switch_is_a_notice_naming_the_key`, which names no
test and matches the `Ok` and `Err` variants. Both variants delegate to one
helper, `outcome_after_a_key_switch`, so a filter cannot name the shared
behaviour. Name the `Ok` variant for both:

- `panel: an outcome for a key no longer shown is a notice` disables the
  key-mismatch filter, which either variant catches; the `Ok` one is the
  happy path and the convention.
- `panel: a key switch drops the upload's sent rows` retains `sent` and
  `submitted` across a switch, and those rows only matter to the echo
  compare, which runs on success. The `Ok` variant is the one that sees it.

## 5. Code fixes

### 5.1 `config_version` mints a phantom dataset

`crates/geode-core/src/schema/mod.rs:219` iterates `doc.value` with no
`config_version` guard. Every other top-level-object reader in the crate
has one: `view.rs:426`, `view.rs:652`, `view.rs:931`, `scopes.rs:49`,
`dimensions.rs:50`, `groupings.rs:44`, `source_config.rs:326`,
`egress_config.rs:96`, and `colour/mod.rs`.

A `datasets.toml` carrying the documented version stamp therefore yields a
`DatasetSpec` named `config_version`: the integer has no `family` and no
`columns`, so family defaults to `Measures`, the columnless branch warns at
`datasets.config_version`, and an empty *measure* dataset is not dropped.
That value reaches `Store::apply_schema`
(`crates/geode-data/src/service.rs:546`), the dataset pick lists in the
object dialog, and the shell's pickable-column walk. The configuration that
avoids the bug is the one the loader complains about, because an absent
stamp warns.

The repo's own `examples/demo-config/datasets.toml` escapes only because
its stamp sits in a comment header rather than a key.

Fix: the same one-line guard, first statement in the loop.

Test: `a_datasets_config_version_header_is_not_a_spurious_diagnostic`,
mirroring `an_egress_config_version_header_is_not_a_spurious_diagnostic`
(`crates/geode-core/src/egress_config.rs:435`) — assert no diagnostics and
one dataset.

Harness: an entry anchoring the guard and mutating it to `if false {`,
naming that test.

### 5.2 The egress worker has no panic containment

`crates/geode-data/src/egress.rs` contains no `catch_unwind`. Its `work`
loop calls `egress.upload` directly, where every other worker in the crate
wraps foreign code: `ingest/fetch.rs:118`, `ingest/runner.rs:368`,
`ingest/subscribe.rs:290`, `ingest/scheduler.rs:159`,
`pricing/worker.rs:146`.

On a transport panic the thread unwinds, so the in-flight job's `answer`
never runs. That breaks the contract stated at `egress.rs:7-11` and in the
crate README, "never both, never neither". The receiver then drops, up to
`EGRESS_QUEUE_BOUND` queued jobs vanish unanswered, and later uploads
answer `stopped` without saying why. The panic also reaches the process
hook, so the artifact says "crash" while the process keeps running.

Fix: wrap the call as `fetch.rs` does, with
`std::panic::catch_unwind(AssertUnwindSafe(…))` around
`geode_core::panic::contained(…)`, answer
`Err("egress '<target>': transport panicked: …")`, and keep the loop alive.

The panic payload is rendered with `ingest::runner::panic_payload_message`,
which is already `pub(crate)`. This deliberately adds no fourth copy of
that helper. The two remaining near-duplicates
(`query/pool.rs:403`, `pricing/worker.rs:109`) differ in signature and in
fallback text, and `pool.rs` bakes "query worker panicked" into its
`format!`, so consolidating them is a separate change with its own
message-text decisions. Recorded in §9.

Test: a `PanickingEgress` fixture in the file's own `#[cfg(test)]` module
beside `GateEgress` (`egress.rs:530`), asserting the upload is answered
exactly once with an error naming the target, and that a following upload
to the same target still succeeds. The fixture is local rather than in
`store/ddl.rs`'s `tests_support`, because only these tests need it; the
`PanickingKind` there is shared across modules and earns its place.

Harness: an entry removing the containment, naming that test.

### 5.3 The status bar allocates on every frame

`crates/geode-shell/src/shell/status.rs:113-117` builds the joined
pending-keystroke string before any check on `pending`, and
`format_keystroke` (`:280-295`) allocates a `Vec<&str>` and a `String` per
keystroke. The result is consumed once, at `:136`.

Fix: build and attach the element only when `pending` is non-empty,
matching the `if let Some(count)` immediately above it.

One observable consequence: while nothing is pending the element is absent
rather than present and empty. If a window test asserts that element exists
while idle, that is a question about whether its presence is a contract,
and it will be raised rather than answered by deleting the assertion.

### 5.4 The which-key sort allocates per comparison

`crates/geode-shell/src/shell/whichkey.rs:53` uses `sort_by_key` with a
key function returning an owned `String`, so `render_keystroke` runs
O(n log n) times per frame while a chord prefix is held.

Fix: `sort_by_cached_key`. No behaviour change.

## 6. Documentation

`docs/current/configuration.md:23-25` lists twelve whole-object roots and
omits `egress`, which `config/merge.rs:19-34` includes and
`merge.rs:214` tests. That sentence is what a desk-config author reads
before overriding a target, and the omission implies field-by-field merge
where the code replaces wholesale.

Fix: add `egress` to the list.

## 7. CI

`.github/workflows/ci.yml` runs formatting, Clippy, tests, benchmark
compilation and the shell `test-support` check. It does not run
`--anchors-only`, although CLAUDE.md requires it before every merge and it
costs 0.19 s.

Fix: a sixth step, `zsh scripts/mutation-check.sh --anchors-only`, after
the `test-support` check.

## 8. Testing and harness

Two new tests and two new harness entries, as §5.1 and §5.2 state.

The linter's own evidence is a before-and-after on this checkout, since a
zsh script has no natural unit test:

- Before the §4 repairs, `--anchors-only` reports exactly 4 hard filter
  findings and 7 `FILTER?` warnings, and exits non-zero.
- After them, 0 hard findings and 7 warnings, and exits zero.
- The summary prints the duplicate and shadowed anchor counts, which become
  the follow-up branch's scope.

Gates before merge: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, and
`zsh scripts/mutation-check.sh --anchors-only`. Iteration uses
`--changed`; the full audit is not required for this branch, because no
entry's anchored code changes except the two whose entries are added here.

## 9. Out of scope

Named so they are not mistaken for oversights.

- **The roughly 90 anchor re-anchorings.** This branch reports them; the
  follow-up fixes them and promotes `DUP` and `SHADOW` to hard failures.
- **The service request loop's containment**, and the liveness signal that
  belongs with it. `handle.rs:296-397` dispatches every request with no
  `catch_unwind`, which is the worse half of §5.2's defect, but each arm
  needs a decision about what it answers on a panic and the shell needs a
  way to learn the thread died. That is its own unit.
- **Consolidating the two remaining panic-message helpers** (§5.2).
- **Everything else in the review**: the silent-wrong-data defects, the
  silence-should-be-a-signal items, the prepared models for shell chrome,
  the shared tile crate, the comment chronology sweep, and the five
  decisions listed as needing a ruling.
