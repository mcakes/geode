# Mutation Harness Linting and Mechanical Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the mutation harness validate the test filter it already
requires, repair the four entries whose filter names no test, and fix five
mechanical defects the 2026-09-25 review verified.

**Architecture:** All harness work lands in the existing `--anchors-only` pass
in `scripts/mutation-check.sh`, which already reads every anchored file and runs
no cargo. `run_mutation` starts recording the package and filter alongside the
anchor; the python pass at the end of the script gains filter and
anchor-uniqueness checks. The code fixes are independent single-site edits in
four crates, each with a test and, where it changes a correctness contract, a
mutation entry.

**Tech Stack:** zsh, python3 (already used by the harness for its anchor pass),
Rust 2024, cargo, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-25-geode-harness-and-mechanical-fixes-design.md`

## Global Constraints

- Every gate must pass before merge: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `zsh scripts/mutation-check.sh --anchors-only`.
- Comments state the local invariant and the failure it prevents. They must not
  cite a task number, phase number, spec section, review finding id or date.
  This is a CLAUDE.md rule and the review found 2,472 comment lines breaking it;
  do not add to them.
- Every new mutation entry names its covering test as the sixth argument to
  `run_mutation`. An entry without one makes `caught` meaningless.
- Harness flags are positional: `--anchors-only` first, then `--changed`, then
  the substring.
- Do not add a fourth copy of the panic-payload helper. `geode-data` already has
  three near-duplicates; reuse `crate::ingest::runner::panic_payload_message`,
  which is already `pub(crate)`.
- Do not introduce libtest `--exact`. It matches the full test path
  (`module::path::fn`) and every filter in the harness is a bare function name,
  so it would match nothing and send all 1,575 entries down the full-suite path.
- Run `zsh scripts/mutation-check.sh --changed` while iterating, never the
  unfiltered audit. A full run is about an hour.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `scripts/mutation-check.sh` | Modify: `run_mutation` record at `:213-216`, the python pass at `:18690-end`, the usage comment block at `:45-62` | Records package and filter per entry; lints filters and anchor uniqueness |
| `crates/geode-core/src/schema/mod.rs` | Modify: the loop at `:218`; add a test in the module at `:1057` | Skips the `config_version` document header like its nine sibling readers |
| `crates/geode-data/src/egress.rs` | Modify: `work` at `:139-154`; add a fixture and test in the module at `:284` | Contains a panicking transport instead of losing the worker and its queue |
| `crates/geode-shell/src/shell/status.rs` | Modify: `:113-117` and `:133-137` | Stops building the pending-keystroke string on every frame |
| `crates/geode-shell/src/shell/whichkey.rs` | Modify: `:53` | Stops allocating a `String` per sort comparison |
| `docs/current/configuration.md` | Modify: `:23-25` | Lists all thirteen whole-object roots, not twelve |
| `.github/workflows/ci.yml` | Modify: add a step after the `test-support` check | Runs the project's own 0.19 s merge gate |

---

### Task 1: Filter linting in `--anchors-only`

The harness checks each entry's anchor twice and its test filter nowhere. An
entry naming a test that does not exist prints one easily-missed `FILTER` line
during a normal run, clears the filter, runs the whole crate suite, and reports
`caught` on an unrelated test. `--anchors-only` returns before filters are
considered, so the merge gate cannot see it at all.

**Files:**
- Modify: `scripts/mutation-check.sh:213-216` (the `anchors_only` record in `run_mutation`)
- Modify: `scripts/mutation-check.sh:18690-end` (the python pass)
- Modify: `scripts/mutation-check.sh:45-62` (the `--anchors-only` usage comment)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `--anchors-only` prints `FILTER`, `FILTERx N` and `FILTER? N` lines,
  counts them in its summary as `N bad filters`, and exits non-zero on
  `FILTER`/`FILTERx`. The NUL record written per entry becomes five fields in
  the order `name, file, from, pkg, filter`; Task 3 reads the same five.

- [ ] **Step 1: Confirm the current baseline, so the change is measurable**

Run: `zsh scripts/mutation-check.sh --anchors-only`

Expected: `checked 1575 anchors: 0 stale, 0 ambiguous` and exit 0. Record the
number; every later step compares against it.

- [ ] **Step 2: Record the package and filter with each anchor**

In `run_mutation`, replace this block:

```zsh
  if (( anchors_only )); then
    printf '%s\0%s\0%s\0' "$name" "$file" "$from" >> "$anchors"
    return 0
  fi
```

with:

```zsh
  if (( anchors_only )); then
    # Five fields per entry, not three: the filter is the field that makes a
    # `caught` verdict mean anything, and nothing checked it before.
    printf '%s\0%s\0%s\0%s\0%s\0' "$name" "$file" "$from" "$pkg" "$filter" >> "$anchors"
    return 0
  fi
```

`pkg` and `filter` are already locals of `run_mutation`, defaulted at its first
line as `pkg="${5:-geode-data}" filter="${6:-}"`.

- [ ] **Step 3: Verify the record change alone breaks the parser**

Run: `zsh scripts/mutation-check.sh --anchors-only`

Expected: a wrong count or a python error, because the parser still reads
stride 3. This confirms the two halves are coupled and the next step is
required. Do not commit here.

- [ ] **Step 4: Teach the python pass to read five fields and lint the filter**

Replace the whole `python3 - "$anchors" <<'PY' … PY` block inside
`if (( anchors_only )); then` with this. The stride changes from 3 to 5 and the
filter checks are new; the stale and ambiguous logic is unchanged.

```python
  python3 - "$anchors" <<'PY' || exit 1
import pathlib, re, sys

raw = pathlib.Path(sys.argv[1]).read_bytes() if pathlib.Path(sys.argv[1]).exists() else b""
fields = raw.split(b"\0")[:-1] if raw else []
entries = [tuple(f.decode() for f in fields[i:i + 5]) for i in range(0, len(fields), 5)]
if not entries:
    print("checked 0 anchors (nothing selected)")
    sys.exit(1)

FN_DECL = re.compile(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")

_fn_cache = {}


def test_fns(pkg):
    """Names of test-attributed functions in a package.

    A filter is what cargo is handed, and cargo matches a substring against
    the test's path. Matching against every `fn` would let a filter naming a
    plain helper pass, so only functions carrying a `test` attribute count.
    """
    if pkg in _fn_cache:
        return _fn_cache[pkg]
    names = set()
    for path in sorted((pathlib.Path("crates") / pkg / "src").rglob("*.rs")):
        try:
            lines = path.read_text().splitlines()
        except OSError:
            continue
        saw_test_attr = False
        for line in lines:
            stripped = line.strip()
            declared = FN_DECL.match(stripped)
            if declared:
                if saw_test_attr:
                    names.add(declared.group(1))
                saw_test_attr = False
            elif stripped.startswith("#["):
                if "test" in stripped:
                    saw_test_attr = True
            elif stripped and not stripped.startswith("//"):
                saw_test_attr = False
    _fn_cache[pkg] = names
    return names


texts = {}
stale = ambiguous = bad_filters = loose = 0
for name, file, anchor, pkg, filt in entries:
    if file not in texts:
        try:
            texts[file] = pathlib.Path(file).read_text()
        except OSError:
            texts[file] = None
    text = texts[file]
    if text is None:
        stale += 1
        print(f"ANCHOR    {name}  <-- file missing: {file}")
        continue
    hits = text.count(anchor)
    if hits == 0:
        stale += 1
        print(f"ANCHOR    {name}  <-- anchor no longer matches; mutation is stale")
    elif hits > 1:
        ambiguous += 1
        print(f"AMBIG x{hits}  {name}  <-- anchor matches {hits} times; only the first is mutated")
    if not filt:
        continue
    matched = sorted(n for n in test_fns(pkg) if filt in n)
    if not matched:
        bad_filters += 1
        print(f"FILTER    {name}  <-- '{filt}' matches no test in {pkg}")
    elif len(matched) > 1 and filt not in matched:
        # The named test never runs, so the verdict comes from whatever else
        # the substring caught — the overlapping-defences lie this harness
        # exists to prevent.
        bad_filters += 1
        print(f"FILTERx {len(matched)}  {name}  <-- '{filt}' matches {len(matched)} tests, none of them exactly")
    elif len(matched) > 1:
        # The named test does run; the siblings only make the entry slower and
        # make "which test caught it" unanswerable.
        loose += 1
        print(f"FILTER? {len(matched)}  {name}  <-- '{filt}' also matches {len(matched) - 1} sibling test(s)")

print(f"checked {len(entries)} anchors: {stale} stale, {ambiguous} ambiguous, {bad_filters} bad filters")
if loose:
    print(f"  {loose} loose filters (the named test runs, siblings run with it)")
sys.exit(1 if stale or ambiguous or bad_filters else 0)
PY
```

- [ ] **Step 5: Run it and confirm the four known findings**

Run: `zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"`

Expected, exactly:

```
FILTER    pricer tile: a commit ignores that its line went away  <-- 'a_commit_whose_line_went_away_is_refused' matches no test in geode-pricer
FILTERx 2  <the geode-shell reload entry>  <-- 'emits_config_reloaded_before_the_frame_notifies' matches 2 tests, none of them exactly
FILTERx 2  panel: an outcome for a key no longer shown is a notice  <-- 'outcome_after_a_key_switch_is_a_notice_naming_the_key' matches 2 tests, none of them exactly
FILTERx 2  panel: a key switch drops the upload's sent rows  <-- 'outcome_after_a_key_switch_is_a_notice_naming_the_key' matches 2 tests, none of them exactly
```

plus seven `FILTER? N` lines, then
`checked 1575 anchors: 0 stale, 0 ambiguous, 4 bad filters` and `exit 1`.

If the counts differ, the `test_fns` scan disagrees with cargo. Debug by
printing `matched` for one known-good entry before changing the rule.

- [ ] **Step 6: Update the usage comment to describe the new checks**

In the `--anchors-only` comment block, after the sentence ending
"a normal run reports these two only for the entries it happens to select",
add:

```zsh
# It also checks the sixth-argument test filter, which nothing checked before:
# an entry whose filter matches no test still prints `caught`, because the
# script clears the filter and falls back to the whole crate suite, so the
# verdict comes from an unrelated test. `FILTER` is a filter that matches
# nothing; `FILTERx N` matches several with none of them named exactly, so the
# intended test never runs; `FILTER? N` names a real test that shares its name
# with siblings, which is only slower and is reported as a warning. libtest's
# `--exact` cannot be used instead: it matches the full `module::path::fn`,
# and every filter here is a bare function name, so it would match nothing.
```

- [ ] **Step 7: Confirm the gates still pass**

Run: `cargo fmt --check && git diff --stat`

Expected: fmt clean (no Rust changed), and the diff touches only
`scripts/mutation-check.sh`.

- [ ] **Step 8: Commit**

```bash
git add scripts/mutation-check.sh
git commit -m "test(harness): lint the mutation filter in the anchors pass

The anchor is checked twice and the test filter nowhere, so an entry
naming a test that does not exist clears its filter, runs the whole crate
suite, and reports caught on an unrelated test. The merge gate could not
see it, because --anchors-only returned before filters were considered.

Reports FILTER for a filter that matches no test and FILTERx for one that
matches several with none named exactly, both hard failures; FILTER? for a
named test that shares its name with siblings, a warning. Four entries
fail today."
```

---

### Task 2: Repair the four broken filters

Every resolution below was determined by reading the mutation against the
candidate tests. None needs a judgment call at implementation time.

**Files:**
- Modify: `scripts/mutation-check.sh` (four `run_mutation` calls)

**Interfaces:**
- Consumes: Task 1's `FILTER`/`FILTERx` reporting.
- Produces: `--anchors-only` exits 0 with `0 bad filters` and seven
  `FILTER?` warnings.

- [ ] **Step 1: Fix the pricer entry**

Find the entry named `pricer tile: a commit ignores that its line went away`.
Change its sixth argument from `a_commit_whose_line_went_away_is_refused` to
`an_editor_whose_line_went_away_closes_with_moved`.

That test is at `crates/geode-pricer/src/tile.rs:3825`. The mutation makes
`self.sheet.index_of(line)` fall back to row 0 when the line is gone, which is
the condition that test drives.

- [ ] **Step 2: Probe that the repaired filter actually catches the mutation**

Run: `zsh scripts/mutation-check.sh "a commit ignores that its line went away"`

Expected: `caught    pricer tile: a commit ignores that its line went away`.

If it reports `SURVIVED`, stop and report it: the entry is then guarding a
contract no test states, which is a finding in its own right and not something
to paper over by naming a different test.

- [ ] **Step 3: Fix the shell reload entry**

Find the entry whose sixth argument is
`emits_config_reloaded_before_the_frame_notifies`. Its mutation removes the
`cx.emit(ShellEvent::ConfigReloaded)` inside `if views_changed`, so the views
path is the one it defends. Change the filter to:

```
a_views_change_emits_config_reloaded_before_the_frame_notifies_its_observers
```

- [ ] **Step 4: Fix both market-data key-switch entries**

Both entries filter on `outcome_after_a_key_switch_is_a_notice_naming_the_key`,
which names no test. The two candidates, `an_ok_…` and `an_err_…`
(`crates/geode-marketdata/src/tile.rs:13803` and `:13812`), both delegate to one
helper, so no filter can name the shared behaviour. Change both to:

```
an_ok_outcome_after_a_key_switch_is_a_notice_naming_the_key
```

`panel: an outcome for a key no longer shown is a notice` disables the
key-mismatch filter, which either variant catches; the `Ok` one is the happy
path. `panel: a key switch drops the upload's sent rows` retains `sent` and
`submitted` across a switch, and those rows only matter to the echo compare,
which runs on success, so the `Ok` variant is the one that sees it.

- [ ] **Step 5: Probe the three repaired entries**

Run:

```bash
zsh scripts/mutation-check.sh "emits_config_reloaded" 2>/dev/null || true
zsh scripts/mutation-check.sh "an outcome for a key no longer shown is a notice"
zsh scripts/mutation-check.sh "a key switch drops the upload's sent rows"
```

Note the first substring matches the entry's *name*, not its filter; if it
selects nothing, run `zsh scripts/mutation-check.sh --anchors-only` and read the
entry name out of the report, then use that.

Expected: `caught` for each. A `SURVIVED` is a finding to report, not to
re-aim around.

- [ ] **Step 6: Confirm the gate is clean**

Run: `zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"`

Expected: `checked 1575 anchors: 0 stale, 0 ambiguous, 0 bad filters`, the
`7 loose filters` line, and `exit 0`.

- [ ] **Step 7: Commit**

```bash
git add scripts/mutation-check.sh
git commit -m "test(harness): name the real test in four mutation entries

One entry named a test that does not exist and three named no test and
matched only by accident. Each is repointed at the test its mutation
actually drives: the pricer editor-closes test, the views half of the
config-reloaded pair, and the Ok variant of the key-switch pair, whose
retained sent rows only matter to the echo compare on success."
```

---

### Task 3: Report duplicate and shadowed anchors as warnings

`replace(…, 1)` mutates the first match, so two entries sharing a file and
anchor mean the second defends nothing. The existing `AMBIG` check counts
occurrences of one anchor and therefore cannot see an anchor that occurs once
while sitting inside a longer anchor another entry uses.

These are warnings here, not failures, so this branch leaves CI green. The
follow-up branch that re-anchors them promotes both to hard failures by adding
them to the `sys.exit` condition.

**Files:**
- Modify: `scripts/mutation-check.sh` (the python pass)

**Interfaces:**
- Consumes: Task 1's five-field record.
- Produces: `DUP` and `SHADOW` lines and two summary counts; exit code
  unchanged.

- [ ] **Step 1: Add the two scans**

In the python pass, add `collections` to the imports so the first line reads:

```python
import collections, pathlib, re, sys
```

Then insert this immediately before the final
`print(f"checked {len(entries)} anchors: …")` line:

```python
# `replace(…, 1)` mutates the first match, so two entries on one (file,
# anchor) mean the second defends nothing.
by_anchor = collections.defaultdict(list)
for name, file, anchor, _pkg, _filt in entries:
    by_anchor[(file, anchor)].append(name)
dup_groups = {k: v for k, v in by_anchor.items() if len(v) > 1}
dup_entries = sum(len(v) for v in dup_groups.values())
for (file, _anchor), names in sorted(dup_groups.items()):
    for shadowed_name in names[1:]:
        print(f"DUP       {shadowed_name}  <-- shares (file, anchor) with {names[0]}")

# An anchor that occurs once but sits inside a longer anchor another entry
# uses. AMBIG counts occurrences of one anchor and cannot see this.
anchors_by_file = collections.defaultdict(set)
for _name, file, anchor, _pkg, _filt in entries:
    anchors_by_file[file].add(anchor)
shadowed_anchors = sorted(
    (file, anchor)
    for file, anchors in anchors_by_file.items()
    for anchor in anchors
    if any(other != anchor and anchor in other for other in anchors)
)
example_of = {}
for name, file, anchor, _pkg, _filt in entries:
    example_of.setdefault((file, anchor), name)
for file, anchor in shadowed_anchors:
    print(f"SHADOW    {example_of[(file, anchor)]}  <-- anchor is a substring of a longer anchor in {file}")
```

- [ ] **Step 2: Extend the summary, leaving the exit condition alone**

Replace the two closing lines:

```python
if loose:
    print(f"  {loose} loose filters (the named test runs, siblings run with it)")
sys.exit(1 if stale or ambiguous or bad_filters else 0)
```

with:

```python
warnings = []
if loose:
    warnings.append(f"{loose} loose filters")
if dup_entries:
    warnings.append(f"{dup_entries} duplicate anchors in {len(dup_groups)} groups")
if shadowed_anchors:
    warnings.append(f"{len(shadowed_anchors)} shadowed anchors")
if warnings:
    print(f"  {', '.join(warnings)}")
    print("  (warnings; see the anchor-uniqueness follow-up)")
# DUP and SHADOW stay warnings until the follow-up re-anchors them; adding
# them here is the one-line change that makes them a gate.
sys.exit(1 if stale or ambiguous or bad_filters else 0)
```

- [ ] **Step 3: Run it and confirm the measured counts**

Run: `zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"`

Expected: `checked 1575 anchors: 0 stale, 0 ambiguous, 0 bad filters`, then
`  7 loose filters, 93 duplicate anchors in 45 groups, 43 shadowed anchors`,
then the follow-up line, then `exit 0`.

Those three numbers were measured on this checkout. If they differ, entries
have changed since; report the new numbers rather than adjusting the code to
match an expectation.

- [ ] **Step 4: Commit**

```bash
git add scripts/mutation-check.sh
git commit -m "test(harness): report duplicate and shadowed anchors

Two entries on one (file, anchor) mean the second mutates the first
occurrence and defends nothing. An anchor that occurs once while sitting
inside a longer anchor another entry uses is invisible to AMBIG, which
counts occurrences of a single anchor.

Warnings rather than failures, so the gate stays green until the branch
that re-anchors them; promoting them is one line in the exit condition.
Measured here: 93 duplicate anchors in 45 groups, 43 shadowed."
```

---

### Task 4: Skip `config_version` in the schema reader

`SchemaSpec::from_doc` is the only top-level-object reader in `geode-core`
without a `config_version` guard. Nine siblings have one:
`view.rs:426`, `:652`, `:931`, `scopes.rs:49`, `dimensions.rs:50`,
`groupings.rs:44`, `source_config.rs:326`, `egress_config.rs:96`, and
`colour/mod.rs`.

A `datasets.toml` carrying the documented version stamp therefore yields a
`DatasetSpec` named `config_version`. The integer has no `family` and no
`columns`, so family defaults to `Measures`, the columnless branch warns at
`datasets.config_version`, and an empty *measure* dataset is not dropped. That
value reaches `Store::apply_schema`, the dataset pick lists, and the shell's
pickable-column walk. The configuration that avoids the bug is the one the
loader complains about, because an absent stamp warns.

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs:218`
- Test: `crates/geode-core/src/schema/mod.rs` (the `mod tests` block at `:1057`)
- Modify: `scripts/mutation-check.sh` (new entry)

**Interfaces:**
- Consumes: nothing.
- Produces: `SchemaSpec::from_doc` ignores a top-level `config_version` key.
  Signature unchanged: `pub fn from_doc(doc: &MergedDoc) -> (SchemaSpec, Vec<Diagnostic>)`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/geode-core/src/schema/mod.rs`. The
local `doc()` helper at `:1061` and the `SAMPLE` constant at `:1065` already
exist; this mirrors `an_egress_config_version_header_is_not_a_spurious_diagnostic`
in `crates/geode-core/src/egress_config.rs:435`.

```rust
    #[test]
    fn a_datasets_config_version_header_is_not_a_spurious_diagnostic() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(&format!("config_version = 1\n{SAMPLE}")));
        assert!(
            diags.is_empty(),
            "config_version is a document header, not a dataset: {diags:?}"
        );
        assert_eq!(
            schema.datasets.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["risk_snapshot"],
            "a version stamp must not mint a dataset"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p geode-core --lib -- a_datasets_config_version_header_is_not_a_spurious_diagnostic`

Expected: FAIL. The diagnostics vector holds a warning at
`datasets.config_version`, and the dataset list is
`["config_version", "risk_snapshot"]`.

- [ ] **Step 3: Add the guard**

In `SchemaSpec::from_doc`, insert as the first statement inside the loop. The
loop body is indented twelve spaces.

```rust
        for (ds_name, ds_value) in &doc.value {
            // A document header, not a dataset. Without this skip the integer
            // becomes a familyless, columnless `DatasetSpec` named
            // `config_version`: the columnless check drops only documents, so
            // it survives, reaches `apply_schema`, and appears in every
            // dataset pick list — and the configuration that avoids it is the
            // one `load_layer` warns about, since an absent stamp warns too.
            if ds_name == "config_version" {
                continue;
            }
```

- [ ] **Step 4: Run the test again**

Run: `cargo test -p geode-core --lib -- a_datasets_config_version_header_is_not_a_spurious_diagnostic`

Expected: PASS.

- [ ] **Step 5: Run the crate suite, because the guard changes a shared reader**

Run: `cargo test -p geode-core --lib`

Expected: all pass. If a test asserted the old behaviour it will fail here;
that assertion was encoding the defect and should be updated, with the change
called out in the commit message.

- [ ] **Step 6: Add the mutation entry**

Append near the other `geode-core` schema entries in
`scripts/mutation-check.sh`:

```zsh
# A version stamp is a document header. Mutated, it becomes a familyless
# dataset that reaches apply_schema and every dataset pick list.
run_mutation "schema: a config_version stamp is not a dataset" \
  crates/geode-core/src/schema/mod.rs \
  '            if ds_name == "config_version" {' \
  '            if false {' \
  geode-core \
  a_datasets_config_version_header_is_not_a_spurious_diagnostic
```

- [ ] **Step 7: Verify the entry is caught and the gate is clean**

Run:

```bash
zsh scripts/mutation-check.sh "a config_version stamp is not a dataset"
zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"
```

Expected: `caught    schema: a config_version stamp is not a dataset`, then
`checked 1576 anchors: 0 stale, 0 ambiguous, 0 bad filters` and `exit 0`. The
count rises by one.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-core/src/schema/mod.rs scripts/mutation-check.sh
git commit -m "fix(core): skip config_version in the schema reader

SchemaSpec::from_doc was the only top-level-object reader without the
guard its nine siblings carry, so a datasets.toml with the documented
version stamp minted a familyless, columnless dataset named
config_version. An empty measure dataset is not dropped, so it reached
apply_schema and every dataset pick list — and the configuration that
avoided it was the one the loader warns about, since an absent stamp
warns too."
```

---

### Task 5: Contain a panicking egress transport

`crates/geode-data/src/egress.rs` contains no `catch_unwind`. Every other
worker in the crate wraps foreign code: `ingest/fetch.rs:118`,
`ingest/runner.rs:368`, `ingest/subscribe.rs:290`, `ingest/scheduler.rs:159`,
`pricing/worker.rs:146`.

On a transport panic the thread unwinds, so the in-flight job's `answer` never
runs. That breaks the contract stated at `egress.rs:7-11` and in the crate
README, "never both, never neither". The receiver then drops, up to
`EGRESS_QUEUE_BOUND` queued jobs vanish unanswered, and later uploads answer
`stopped` without saying why.

**Files:**
- Modify: `crates/geode-data/src/egress.rs:139-154` (`work`)
- Test: `crates/geode-data/src/egress.rs` (the `#[cfg(test)]` module at `:284`)
- Modify: `scripts/mutation-check.sh` (new entry)

**Interfaces:**
- Consumes: `crate::ingest::runner::panic_payload_message(&(dyn Any + Send)) -> String`,
  already `pub(crate)`.
- Produces: `work` answers every job exactly once and survives a panicking
  transport. `Egress` is unchanged:
  `fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `crates/geode-data/src/egress.rs`, beside
`GateEgress` at `:530`. `SyncSender`, `Receiver` and `Mutex` are already
imported by that module.

```rust
    /// The message a [`PanickingEgress`] transport panics with. A constant so
    /// the assertion and the panic that produced it cannot drift apart.
    const UPLOAD_PANIC: &str = "the transport fell over";

    /// An egress whose `upload` PANICS rather than answering `Err` — the
    /// failure a real transport has that a `Result` does not describe. The
    /// boundary is only observable through the panic it contains, so the
    /// fixture has to be the thing that panics.
    struct PanickingEgress {
        panics_left: usize,
    }

    impl Egress for PanickingEgress {
        fn upload(&mut self, _target: &str, _bytes: Vec<u8>) -> Result<(), AdapterError> {
            if self.panics_left > 0 {
                self.panics_left -= 1;
                panic!("{UPLOAD_PANIC}");
            }
            Ok(())
        }
    }
```

Then the test itself, in the same module:

```rust
    #[test]
    fn a_panicking_transport_answers_the_upload_and_keeps_the_worker() {
        let (events, rx) = event_sink();
        let workers = workers_over(PanickingEgress { panics_left: 1 }, events);

        assert!(workers.upload(upload_job("first")), "the first upload is admitted");
        let first = next_upload(&rx);
        let message = first.result.expect_err("a panicking transport is an error");
        assert!(
            message.contains(UPLOAD_PANIC),
            "the answer carries the panic payload: {message}"
        );

        assert!(
            workers.upload(upload_job("second")),
            "the worker survived, so the next upload is still admitted"
        );
        let second = next_upload(&rx);
        assert!(
            second.result.is_ok(),
            "the second upload succeeds: {:?}",
            second.result
        );
    }
```

`event_sink`, `workers_over`, `upload_job` and `next_upload` stand for the
helpers this module already uses to drive `EgressWorkers`. Before writing the
test, read the existing tests around `:284-530` and use their real names and
shapes; if a helper does not exist, build the workers the way the neighbouring
test does rather than inventing an abstraction.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p geode-data --lib -- a_panicking_transport_answers_the_upload_and_keeps_the_worker`

Expected: FAIL. The worker thread unwinds, the first job is never answered, and
the test blocks on `next_upload` until its receive times out or the channel
disconnects.

- [ ] **Step 3: Contain the panic**

Replace the body of `work`:

```rust
fn work(name: String, mut egress: Box<dyn Egress>, jobs: Receiver<Job>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        let result = egress
            .upload(&job.address, job.bytes)
            .map_err(|e| format!("egress '{name}': {e}"));
        answer(
```

with:

```rust
fn work(name: String, mut egress: Box<dyn Egress>, jobs: Receiver<Job>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        // A transport is foreign code, so a panic here is a failure of this
        // upload rather than of the worker. Uncontained it would unwind past
        // `answer`, breaking the one-answer-per-upload contract, drop the
        // receiver, and strand every queued job unanswered.
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| egress.upload(&job.address, job.bytes))
        })) {
            Ok(outcome) => outcome.map_err(|e| format!("egress '{name}': {e}")),
            Err(payload) => Err(format!(
                "egress '{name}': transport panicked: {}",
                crate::ingest::runner::panic_payload_message(&*payload)
            )),
        };
        answer(
```

`job.bytes` is moved into the closure, so the `answer` call below must already
be using `job.document`, `job.document_key`, `job.key` and `job.tag` only.
Confirm that before compiling; if `job.bytes` is read after this point, bind
what is needed before the closure.

- [ ] **Step 4: Run the test again**

Run: `cargo test -p geode-data --lib -- a_panicking_transport_answers_the_upload_and_keeps_the_worker`

Expected: PASS.

- [ ] **Step 5: Run the crate suite**

Run: `cargo test -p geode-data --lib`

Expected: all pass, including the existing egress tests that assert the
one-answer contract on the non-panicking paths.

- [ ] **Step 6: Re-anchor the existing entry whose line this fix moved**

`scripts/mutation-check.sh` already has an entry named
`egress: an adapter error answers Err naming the target`, anchored on

```
            .map_err(|e| format!("egress '"'"'{name}'"'"': {e}"));
```

which is the exact line Step 3 folded into the `Ok` arm. Left alone it is a
stale anchor and `--anchors-only` will exit 1. Re-aim it at the site that now
decides, keeping its meaning: the answer must name the target.

Change that entry's `from` to:

```
            Ok(outcome) => outcome.map_err(|e| format!("egress '"'"'{name}'"'"': {e}")),
```

and its `to` to:

```
            Ok(outcome) => outcome.map_err(|e| e.to_string()),
```

Leave its package and filter alone. `'"'"'` is how this script spells a literal
single quote inside a single-quoted anchor; the entry already uses it.

- [ ] **Step 7: Add the containment entry**

Append beside the other `geode-data` egress entries. This anchors the whole
match and replaces it with the pre-fix direct call, which is the pattern
`pool: a panicking query does not wedge its view` uses — a mutation that must
still compile, or `caught` would mean a build error rather than a test.

```zsh
# A transport panic must fail its own upload, not the worker. Mutated, it
# unwinds past `answer`, so the job is never answered and the dropped
# receiver strands every queued upload.
run_mutation "egress: a panicking transport is contained" \
  crates/geode-data/src/egress.rs \
  '        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| egress.upload(&job.address, job.bytes))
        })) {
            Ok(outcome) => outcome.map_err(|e| format!("egress '"'"'{name}'"'"': {e}")),
            Err(payload) => Err(format!(
                "egress '"'"'{name}'"'"': transport panicked: {}",
                crate::ingest::runner::panic_payload_message(&*payload)
            )),
        };' \
  '        let result = egress
            .upload(&job.address, job.bytes)
            .map_err(|e| format!("egress '"'"'{name}'"'"': {e}"));' \
  geode-data \
  a_panicking_transport_answers_the_upload_and_keeps_the_worker
```

- [ ] **Step 8: Verify both entries are caught**

Run:

```bash
zsh scripts/mutation-check.sh "a panicking transport is contained"
zsh scripts/mutation-check.sh "an adapter error answers Err naming the target"
```

Expected: `caught` for both.

If the first reports `caught` suspiciously fast, confirm the mutated crate
actually compiled: a mutation that does not build reports `caught` for a build
error, not a test. The harness header records that trap from a previous entry.

- [ ] **Step 9: Confirm the gate, and expect one more shadowed anchor**

Run: `zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"`

Expected: `checked 1577 anchors: 0 stale, 0 ambiguous, 0 bad filters` and
`exit 0`, with the warning line now reading
`7 loose filters, 93 duplicate anchors in 45 groups, 44 shadowed anchors`.

The shadow count rises by one on purpose: Step 6's re-anchored line sits inside
Step 7's whole-match anchor. That is a benign shadow — the short anchor still
occurs exactly once, so it still mutates the intended site — and it is a
warning, not a failure.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-data/src/egress.rs scripts/mutation-check.sh
git commit -m "fix(data): contain a panicking egress transport

The egress worker was the one background boundary in the crate with no
catch_unwind, so a transport panic unwound past answer: the in-flight
upload was never answered, breaking the never-both-never-neither
contract, and dropping the receiver stranded every queued job while later
uploads answered stopped without saying why.

Shaped like the fetch worker beside it, and reusing runner's existing
panic_payload_message rather than adding a fourth copy of it. The
adapter-error entry is re-aimed at the Ok arm the message moved into
rather than deleted, so it still guards that the answer names the target."
```

---

### Task 6: Two per-frame allocations in the shell chrome

Both sites allocate on every frame for values that rarely change. Neither
changes behaviour except as noted.

**Files:**
- Modify: `crates/geode-shell/src/shell/status.rs:113-117` and `:133-137`
- Modify: `crates/geode-shell/src/shell/whichkey.rs:53`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks rely on.

- [ ] **Step 1: Stop building the pending string when nothing is pending**

In `crates/geode-shell/src/shell/status.rs`, delete this binding:

```rust
    let pending_text = pending
        .iter()
        .map(format_keystroke)
        .collect::<Vec<_>>()
        .join(" ");
```

and replace the element that consumed it:

```rust
    bar = bar.left(
        div()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .child(pending_text),
    );
```

with:

```rust
    // Built only when there is something to show: `format_keystroke`
    // allocates a `Vec` and a `String` per keystroke, and this ran on every
    // frame the app ever painted, pending or not.
    if !pending.is_empty() {
        let pending_text = pending
            .iter()
            .map(format_keystroke)
            .collect::<Vec<_>>()
            .join(" ");
        bar = bar.left(
            div()
                .font_family(fonts::MONO)
                .text_color(theme.muted_foreground)
                .child(pending_text),
        );
    }
```

- [ ] **Step 2: Run the shell suite, watching for a presence assertion**

Run: `cargo test -p geode-shell --lib`

Expected: all pass.

If a test fails because it asserted that element exists while nothing is
pending, **stop and report it**. Whether an always-present empty element is a
contract is a question for Matthew, not something to settle by deleting the
assertion. The rest of this task can proceed while that is open.

- [ ] **Step 3: Stop allocating per sort comparison in which-key**

In `crates/geode-shell/src/shell/whichkey.rs`, replace:

```rust
    result.sort_by_key(|(key, _)| render_keystroke(key));
```

with:

```rust
    // Cached: `render_keystroke` returns an owned `String`, and `sort_by_key`
    // calls its key function O(n log n) times — on every frame a chord prefix
    // is held.
    result.sort_by_cached_key(|(key, _)| render_keystroke(key));
```

- [ ] **Step 4: Verify both, and that the ordering did not change**

Run: `cargo test -p geode-shell --lib && cargo clippy -p geode-shell --all-targets -- -D warnings`

Expected: all pass. `sort_by_cached_key` is stable and orders identically, so
any which-key ordering test must still pass; a failure means the key function
is not pure and that is the finding.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/shell/status.rs crates/geode-shell/src/shell/whichkey.rs
git commit -m "perf(shell): stop two per-frame allocations in the chrome

The status bar joined the pending-keystroke string before checking whether
anything was pending, so it allocated a Vec and a String per keystroke on
every frame the app ever painted. The which-key sort used sort_by_key with
a key function returning an owned String, which sort_by_key calls
O(n log n) times per frame while a prefix is held."
```

---

### Task 7: The missing whole-object root, and the gate in CI

`docs/current/configuration.md:23-25` lists twelve whole-object roots and omits
`egress`, which `crates/geode-core/src/config/merge.rs:19-34` includes and
`merge.rs:214` tests. That sentence is what a desk-config author reads before
overriding a target, and the omission implies field-by-field merge where the
code replaces wholesale.

CI runs formatting, Clippy, tests, benchmark compilation and the shell
`test-support` check, but not `--anchors-only`, although CLAUDE.md requires it
before every merge and it costs 0.19 s.

**Files:**
- Modify: `docs/current/configuration.md:23-25`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Tasks 1 to 5, whose combined effect must leave `--anchors-only`
  exiting 0. Do this task last.
- Produces: nothing.

- [ ] **Step 1: Add `egress` to the whole-object list**

Replace:

```markdown
Top-level entries in `views`, `view_presentation`, `dataset_presentation`,
`layouts`, `groupings`, `scopes`, `datasets`, `sources`, `dimensions`,
`colours`, `pricer_views`, and `overrides` replace whole named objects.
```

with:

```markdown
Top-level entries in `views`, `view_presentation`, `dataset_presentation`,
`layouts`, `groupings`, `scopes`, `datasets`, `sources`, `egress`,
`dimensions`, `colours`, `pricer_views`, and `overrides` replace whole named
objects.
```

The order matches `atomic_depth`'s match arms, so the two read the same way.

- [ ] **Step 2: Verify the list now matches the code exactly**

Run:

```bash
grep -n 'Some(1)' -B 14 crates/geode-core/src/config/merge.rs | grep -oE '"[a-z_]+"' | tr -d '"' | sort > /tmp/code-roots
sed -n '23,26p' docs/current/configuration.md | grep -oE '`[a-z_]+`' | tr -d '`' | sort > /tmp/doc-roots
diff /tmp/code-roots /tmp/doc-roots && echo "lists agree"
```

Expected: `lists agree`.

- [ ] **Step 3: Add the anchors gate to CI**

In `.github/workflows/ci.yml`, after the step that runs the shell
`test-support` check, add:

```yaml
      - name: Mutation anchors
        run: zsh scripts/mutation-check.sh --anchors-only
```

Read the surrounding steps first and match their `name`/`run` style and
indentation. The script needs `zsh` and `python3`, both present on the
`macos-latest` and `windows-latest` runners this job already uses; if the
Windows runner has no `zsh`, guard the step with the same `if:` the job's other
platform-specific steps use and say so in the commit message.

- [ ] **Step 4: Run every gate the way CI will**

Run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```

Expected: every one clean, and the last exits 0 with
`checked 1577 anchors: 0 stale, 0 ambiguous, 0 bad filters` and the warning line
`7 loose filters, 93 duplicate anchors in 45 groups, 44 shadowed anchors`.

- [ ] **Step 5: Commit**

```bash
git add docs/current/configuration.md .github/workflows/ci.yml
git commit -m "docs(config): list egress as a whole-object root, gate anchors in CI

The whole-object list named twelve of the thirteen roots atomic_depth
actually replaces wholesale, omitting egress — the one sentence a desk
config author reads before overriding a target.

CI now runs the anchors pass CLAUDE.md already requires before every
merge; it costs 0.19 s and was the only stated gate not enforced."
```

---

## Self-Review

**Spec coverage.** Every section of the spec maps to a task: §3's four checks to
Tasks 1 and 3, §4's four repairs to Task 2, §5.1 to Task 4, §5.2 to Task 5,
§5.3 and §5.4 to Task 6, §6 and §7 to Task 7. §2 (why not `--exact`) is carried
into the Global Constraints and Task 1's usage comment. §8's acceptance
criteria appear as the expected outputs of Task 1 Step 5, Task 2 Step 6 and
Task 3 Step 3. §9's out-of-scope list is not implemented, by design, and Task 3
leaves the promotion hook it names.

**Placeholders.** One deliberate soft spot, flagged rather than hidden: Task 5
Step 1 names four test helpers (`event_sink`, `workers_over`, `upload_job`,
`next_upload`) as stand-ins and instructs the implementer to read the
neighbouring tests for their real names. Inventing helper names here would be
worse than saying so, because `egress.rs`'s test module was read only around
`GateEgress`. Every other code block is literal.

**Type consistency.** `SchemaSpec::from_doc` keeps its signature;
`schema.datasets` is `Vec<DatasetSpec>` and `DatasetSpec::name` is `String`, so
Task 4's `iter().map(|d| d.name.as_str())` compiles. `Egress::upload` is
`(&mut self, &str, Vec<u8>) -> Result<(), AdapterError>`, matched by
`PanickingEgress`. `panic_payload_message` takes `&(dyn Any + Send)`, so
Task 5 passes `&*payload` from the `Box<dyn Any + Send>` that `catch_unwind`
returns. The NUL record is five fields in the order `name, file, from, pkg,
filter` in Task 1 and read in that order in Task 3.

**Anchor counts.** 1,575 before Task 4, 1,576 after it, 1,577 after Task 5.
Each task states the count it expects. The shadowed-anchor warning is 43 at
Task 3 and 44 after Task 5, because Task 5 re-anchors an existing entry onto a
line that sits inside its own new whole-match anchor.

**Two defects caught in this review and fixed inline.** Task 5's mutation
originally replaced only the first line of the `match`, which would have left an
unbalanced expression that does not compile — `caught` would then have meant a
build error. It now anchors the whole match and reverts to the direct call, the
pattern `pool: a panicking query does not wedge its view` already uses. And
Task 5's fix moves the line an existing entry anchors, which would have left a
stale anchor and a red gate; Step 6 re-aims that entry rather than deleting it.

**Verification commands were run, not assumed.** Task 7 Step 2's
code-versus-doc diff was executed against the current tree: it lists thirteen
roots in the code and twelve in the guide, differing only by `egress`.
