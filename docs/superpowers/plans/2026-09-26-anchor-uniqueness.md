# Mutation Anchor Checker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `scripts/mutation-check.sh` fail on the entry defects that can actually make a verdict lie (a redundant entry, a missing filter, a no-op mutation, a mutation that does not compile), stop printing ~119 warning lines that flag correct entries, and give the checker its own tests.

**Architecture:** The `--anchors-only` python heredoc moves into `scripts/mutation_anchors.py` as a pure `check(entries, root)` function plus a CLI, with a stdlib `unittest` suite beside it. The zsh runner records six fields per entry (the replacement is new) and hands them to the module. Mutation runs gain a `BUILD` verdict and a `--build-check` audit mode that compiles each mutation without running tests.

**Tech Stack:** zsh, Python 3 stdlib only (`unittest`, `pathlib`, `re`, `collections`), GitHub Actions.

**Spec:** No separate spec. Scope is the anchor-uniqueness follow-up named in the 2026-09-25 review (`docs/superpowers/reviews/2026-09-25/`), the A+B final review's deferred items 6-11, and two rulings Matthew made on 2026-09-26, recorded here:

1. **DUP and SHADOW are redefined, not promoted.** Measured on main at 36ded0dd (1,952 entries): of 61 groups sharing a (file, anchor), 57 carry *different* replacements (legitimate: different mutations of one line), 3 repeat a replacement under a *different* test (a second test catching the same mutation), and 1 repeats replacement AND test (truly redundant). SHADOW cannot indicate a defect: AMBIG already fails any anchor that occurs more than once, so an anchor nested inside a longer one occurs exactly once, inside it — a sub-span of the same site. Ruling: fail only on an identical (file, anchor, replacement, filter); report the same mutation under a different test as information; delete SHADOW.
2. **A mutation that does not compile is not a catch.** Add a `BUILD` verdict to mutation runs (non-zero exit) and a `--build-check` compile-only audit mode.

## Global Constraints

- Python is stdlib only; no pytest, no third-party imports. Every file read passes `encoding="utf-8"`.
- `--anchors-only` still runs no Cargo command and edits no source file.
- Flags stay positional: `[--anchors-only | --build-check] [--changed[=REF]] [substring]`. A flag in the substring slot still exits 2.
- Never run `zsh scripts/mutation-check.sh --changed` (main moves; it selects other branches' entries and runs for hours). Probe entries by NAME SUBSTRING only. Never run an unfiltered mutation run or an unfiltered `--build-check`; the controller runs the audit detached.
- When waiting on a harness process, match it with `pgrep -f 'mutation-che[c]k'` (the bracket form cannot match its own shell).
- A comment states the local invariant and the failure it prevents. No task numbers, review ids, dates or "Task N" in code comments or the script's header.
- Behaviour changes update `CLAUDE.md`'s command comment and `docs/current/data-path.md`'s harness paragraph in the same task.
- Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Output drift in the extraction.** Task 1 must print byte-identical output to the heredoc it replaces on the real entry set; any changed verdict means the port is wrong. (Pinned: Task 1 Step 6 diffs the two outputs.)
2. **Test-attribute recognition.** `#[cfg(any(test, feature = "test-support"))]` and `#[cfg_attr(not(test), allow(dead_code))]` are NOT test attributes; `#[test]`, `#[gpui::test]`, `#[gpui::test(iterations = 3)]` are; a second attribute or a doc comment between the attribute and `fn` keeps the marker. (Pinned: Task 1 tests.)
3. **A cargo failure that is a test failure must still read `caught`.** `BUILD` must key on the compile-failure signature, not on "non-zero exit"; a failing assertion prints `test result: FAILED` and no `could not compile`. (Pinned: Task 3 Step 2 runs a real caught entry and a deliberately broken one.)
4. **The file in flight is restored after `--build-check`,** including on a compile failure and on SIGTERM. (Pinned: Task 3 Step 5 checks `git status` after each run.)
5. **Windows / non-UTF-8 locale:** a source file containing non-ASCII must be read, not raise. (Pinned: Task 1 test `test_reads_non_ascii_source`.)

---

## File Structure

- Create `scripts/mutation_anchors.py` — the anchor/filter checker: `Entry` record, `test_fns`, `check`, `main`. One responsibility: static validation of the entry table against the source tree.
- Create `scripts/test_mutation_anchors.py` — unittest suite over synthetic trees in a temp dir.
- Modify `scripts/mutation-check.sh` — header comments, six-field record, call the module, `BUILD` verdict, `--build-check` mode, repaired ingest entry, the launch-entries comment near the end.
- Modify `.github/workflows/ci.yml` — anchors step moves right after checkout; unittest step added.
- Modify `CLAUDE.md`, `docs/current/data-path.md` — command comment and harness paragraph.
- Modify `crates/geode-data/src/egress.rs` (tests module only) — one adapter fixture instead of two.

---

### Task 1: Extract the checker into a tested module (behaviour-preserving)

**Files:**
- Create: `scripts/mutation_anchors.py`
- Create: `scripts/test_mutation_anchors.py`
- Modify: `scripts/mutation-check.sh` (the `printf` in `run_mutation`'s `anchors_only` branch, ~line 164; the heredoc block at the end, ~lines 21907-22039; the `anchors=` comment ~line 58)

**Interfaces:**
- Produces: `scripts/mutation_anchors.py` with
  - `Entry = collections.namedtuple("Entry", "name file anchor replacement pkg filter")`
  - `def read_entries(raw: bytes) -> list[Entry]` — splits NUL-separated six-field records.
  - `def test_fns(root: pathlib.Path, pkg: str) -> set[str]` — test-attributed fn names under `root/crates/<pkg>/src`.
  - `def check(entries: list[Entry], root: pathlib.Path) -> tuple[list[str], int]` — output lines and exit code.
  - `def main(argv: list[str]) -> int` — `python3 scripts/mutation_anchors.py <record-file>`; prints lines, returns the code. Root is the current directory (the zsh script has already `cd`'d to the repo top).
- The zsh record becomes six fields: `name, file, anchor, replacement, pkg, filter`.

- [ ] **Step 1: Capture the baseline output**

```bash
zsh scripts/mutation-check.sh --anchors-only > "$SCRATCH/anchors-before.txt"; echo "exit $?"
```
Expected: `exit 0`, last lines `checked 1952 anchors: 0 stale, 0 ambiguous, 0 bad filters` (the count may differ if main moved; the file is the baseline either way). `$SCRATCH` is the scratchpad directory given in your brief.

- [ ] **Step 2: Write the failing tests**

Create `scripts/test_mutation_anchors.py`. Each test builds a tree under `tempfile.TemporaryDirectory()` with `crates/<pkg>/src/lib.rs` and calls `check` directly. Use this helper and these cases (exact assertions on the verdict prefix of the line and on the exit code):

```python
import pathlib
import tempfile
import unittest

import mutation_anchors as ma


def tree(files):
    """Write {relative path: text} under a fresh temp root; return (tmp, root)."""
    tmp = tempfile.TemporaryDirectory()
    root = pathlib.Path(tmp.name)
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return tmp, root


LIB = "crates/p/src/lib.rs"


def entry(name="e", file=LIB, anchor="let x = 1;", replacement="let x = 2;", pkg="p", filt="t_one"):
    return ma.Entry(name, file, anchor, replacement, pkg, filt)


SRC = """
fn f() { let x = 1; }
#[test]
fn t_one() {}
#[gpui::test]
fn t_two() {}
#[gpui::test(iterations = 3)]
fn t_three() {}
"""


class Check(unittest.TestCase):
    def run_check(self, files, entries):
        tmp, root = tree(files)
        self.addCleanup(tmp.cleanup)
        return ma.check(entries, root)

    def test_a_clean_entry_passes(self):
        lines, code = self.run_check({LIB: SRC}, [entry()])
        self.assertEqual(code, 0)
        self.assertEqual(lines[-1] if not lines[-1].startswith("  ") else lines[0],
                         "checked 1 anchors: 0 stale, 0 ambiguous, 0 bad filters")

    def test_an_empty_selection_fails(self):
        lines, code = self.run_check({LIB: SRC}, [])
        self.assertEqual((lines, code), (["checked 0 anchors (nothing selected)"], 1))

    def test_a_missing_file_is_stale(self):
        lines, code = self.run_check({LIB: SRC}, [entry(file="crates/p/src/gone.rs")])
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("ANCHOR    e  <-- file missing"))

    def test_an_anchor_that_no_longer_matches_is_stale(self):
        lines, code = self.run_check({LIB: SRC}, [entry(anchor="let y = 1;")])
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("ANCHOR    e"))

    def test_an_anchor_matching_twice_is_ambiguous(self):
        lines, code = self.run_check({LIB: SRC + "fn g() { let x = 1; }\n"}, [entry()])
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("AMBIG x2  e"))

    def test_gpui_test_attributes_count_as_tests(self):
        for filt in ("t_two", "t_three"):
            lines, code = self.run_check({LIB: SRC}, [entry(filt=filt)])
            self.assertEqual(code, 0, (filt, lines))

    def test_cfg_attributes_mentioning_test_do_not_make_a_test(self):
        src = SRC + (
            '#[cfg(any(test, feature = "test-support"))]\nfn for_tests() {}\n'
            "#[cfg_attr(not(test), allow(dead_code))]\nfn helper() {}\n"
        )
        for filt in ("for_tests", "helper"):
            lines, code = self.run_check({LIB: src}, [entry(filt=filt)])
            self.assertEqual(code, 1, (filt, lines))
            self.assertTrue(lines[0].startswith("FILTER    e"), lines)

    def test_a_second_attribute_or_doc_comment_keeps_the_test_marker(self):
        src = SRC + "#[test]\n/// doc\n#[should_panic]\nfn t_panics() {}\n"
        lines, code = self.run_check({LIB: src}, [entry(filt="t_panics")])
        self.assertEqual(code, 0, lines)

    def test_a_filter_matching_several_tests_without_an_exact_name_fails(self):
        lines, code = self.run_check({LIB: SRC}, [entry(filt="t_t")])
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("FILTERx 2  e"), lines)

    def test_an_exact_name_that_prefixes_siblings_is_a_warning(self):
        src = SRC + "#[test]\nfn t_one_more() {}\n"
        lines, code = self.run_check({LIB: src}, [entry(filt="t_one")])
        self.assertEqual(code, 0)
        self.assertTrue(lines[0].startswith("FILTER? 2  e"), lines)

    def test_reads_non_ascii_source(self):
        src = SRC.replace("fn f()", "// chevron ▸ and em dash —\nfn f()")
        lines, code = self.run_check({LIB: src}, [entry()])
        self.assertEqual(code, 0, lines)


class Records(unittest.TestCase):
    def test_six_field_records_round_trip(self):
        raw = b"n\0f\0a\0r\0p\0t\0" b"n2\0f2\0a2\0r2\0p2\0\0"
        self.assertEqual(
            ma.read_entries(raw),
            [ma.Entry("n", "f", "a", "r", "p", "t"), ma.Entry("n2", "f2", "a2", "r2", "p2", "")],
        )


if __name__ == "__main__":
    unittest.main()
```

(`test_a_clean_entry_passes` asserts the summary line; write it plainly as `self.assertIn("checked 1 anchors: 0 stale, 0 ambiguous, 0 bad filters", lines)` if that reads better — the contract is that the summary line is present verbatim.)

- [ ] **Step 3: Run the tests to verify they fail**

Run: `python3 -m unittest discover -s scripts -p 'test_*.py'`
Expected: ERROR, `ModuleNotFoundError: No module named 'mutation_anchors'`.

- [ ] **Step 4: Write the module by moving the heredoc**

Create `scripts/mutation_anchors.py`. Move the heredoc's logic in, verbatim in behaviour, with these mechanical changes only:
- every path is resolved against `root` (`root / "crates" / pkg / "src"`, `root / file`);
- `read_entries` splits six fields (`fields[i:i + 6]`);
- `check` builds a `lines` list instead of printing, and returns `(lines, code)` where `code` is the heredoc's `sys.exit` argument;
- `main(argv)` reads `argv[1]` (a missing file reads as empty, as today), calls `check(entries, pathlib.Path("."))`, prints each line, returns the code; the module ends with `if __name__ == "__main__": sys.exit(main(sys.argv))`.

Keep DUP, SHADOW and the "(warnings; see the anchor-uniqueness follow-up)" line exactly as they are in this task; Task 2 changes them. Give the module a docstring stating what it checks and that it runs no Cargo command. Keep `test_fns`'s docstring.

In `scripts/mutation-check.sh`:
- the `anchors_only` branch of `run_mutation` becomes
  ```zsh
    printf '%s\0%s\0%s\0%s\0%s\0%s\0' "$name" "$file" "$from" "$to" "$pkg" "$filter" >> "$anchors"
  ```
  and its comment says the record carries the replacement so redundant entries can be recognised;
- the comment at the `anchors=` line names the six fields;
- the final block becomes
  ```zsh
  if (( anchors_only )); then
    # Static checks over every selected entry; see scripts/mutation_anchors.py.
    python3 scripts/mutation_anchors.py "$anchors" || exit 1
  fi
  ```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `python3 -m unittest discover -s scripts -p 'test_*.py' -v`
Expected: all pass.

- [ ] **Step 6: Prove the port is byte-identical on the real entries**

```bash
zsh scripts/mutation-check.sh --anchors-only > "$SCRATCH/anchors-after.txt"; echo "exit $?"
diff "$SCRATCH/anchors-before.txt" "$SCRATCH/anchors-after.txt" && echo IDENTICAL
zsh scripts/mutation-check.sh --anchors-only "launch: blotter" ; echo "exit $?"
```
Expected: `exit 0`, `IDENTICAL`, and the substring run prints `checked 2 anchors` with exit 0 (it also prints the DUP line for that pair — correct for this task).

- [ ] **Step 7: Add the unittest step to CI and commit**

In `.github/workflows/ci.yml`, directly after `- uses: actions/checkout@...` (read the file for the exact line), add:
```yaml
      - name: Mutation checker tests
        if: runner.os != 'Windows'
        run: python3 -m unittest discover -s scripts -p "test_*.py"
```
(`python3` is not guaranteed on the Windows hosted runner; the zsh gate beside it is already macOS-only.)

```bash
git add scripts/mutation_anchors.py scripts/test_mutation_anchors.py scripts/mutation-check.sh .github/workflows/ci.yml
git commit -m "refactor(scripts): move the anchor checker into a tested python module

The merge gate was ~115 lines of python inside an 18k-line zsh heredoc
with no test of its own; its attribute-matching bug was found only by a
human reviewer. Output is byte-identical on the full entry table. The
record now carries each entry's replacement, which the next change needs.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Fail on entries that cannot mean what they claim; stop flagging correct ones

**Files:**
- Modify: `scripts/mutation_anchors.py`
- Modify: `scripts/test_mutation_anchors.py`
- Modify: `scripts/mutation-check.sh` (header lines ~9-20; the ingest entry "ingest: a stale skip does not start the strip" ~line 14822; the comment above the two "launch: blotter" entries ~line 21878)
- Modify: `CLAUDE.md` (the `--anchors-only` command comment), `docs/current/data-path.md` (~line 521, the harness paragraph)

**Interfaces:**
- Consumes: `Entry`, `check` from Task 1.
- Produces: new verdict strings, each a fixed-width 10-character prefix like the existing ones:
  - `REDUNDANT {name}  <-- repeats {first}: same anchor, replacement and test` — hard failure.
  - `NOFILTER  {name}  <-- names no detecting test` — hard failure (empty sixth field).
  - `NOOP      {name}  <-- replacement equals the anchor; nothing is mutated` — hard failure.
  - `ALSO      {name}  <-- same mutation as {first}, detected by '{filter}'` — information, exit unaffected.
  - SHADOW and DUP removed. The summary's bad-count line becomes `checked N anchors: S stale, A ambiguous, F bad filters, R bad entries`, where R counts REDUNDANT + NOFILTER + NOOP. The warnings line lists loose filters and `M same-mutation entries` (the ALSO count) and no longer mentions a follow-up.

- [ ] **Step 1: Write the failing tests** (append to `Check`)

```python
    def test_a_repeated_mutation_under_the_same_test_is_redundant(self):
        lines, code = self.run_check({LIB: SRC}, [entry(name="a"), entry(name="b")])
        self.assertEqual(code, 1)
        self.assertIn("REDUNDANT b  <-- repeats a: same anchor, replacement and test", lines)

    def test_a_repeated_mutation_under_another_test_is_information(self):
        lines, code = self.run_check({LIB: SRC}, [entry(name="a"), entry(name="b", filt="t_two")])
        self.assertEqual(code, 0, lines)
        self.assertIn("ALSO      b  <-- same mutation as a, detected by 't_two'", lines)

    def test_different_mutations_of_one_anchor_are_silent(self):
        lines, code = self.run_check(
            {LIB: SRC}, [entry(name="a"), entry(name="b", replacement="let x = 3;")]
        )
        self.assertEqual(code, 0)
        self.assertFalse([l for l in lines if l.startswith(("DUP", "ALSO", "REDUNDANT"))], lines)

    def test_a_nested_anchor_is_silent(self):
        lines, code = self.run_check(
            {LIB: SRC},
            [entry(name="a"), entry(name="b", anchor="x = 1", replacement="x = 4")],
        )
        self.assertEqual(code, 0)
        self.assertFalse([l for l in lines if l.startswith("SHADOW")], lines)

    def test_an_entry_without_a_filter_fails(self):
        lines, code = self.run_check({LIB: SRC}, [entry(filt="")])
        self.assertEqual(code, 1)
        self.assertIn("NOFILTER  e  <-- names no detecting test", lines)

    def test_a_replacement_equal_to_its_anchor_fails(self):
        lines, code = self.run_check({LIB: SRC}, [entry(replacement="let x = 1;")])
        self.assertEqual(code, 1)
        self.assertIn("NOOP      e  <-- replacement equals the anchor; nothing is mutated", lines)

    def test_the_summary_counts_bad_entries(self):
        lines, _ = self.run_check({LIB: SRC}, [entry(name="a"), entry(name="b"), entry(name="c", filt="")])
        self.assertIn("checked 3 anchors: 0 stale, 0 ambiguous, 0 bad filters, 2 bad entries", lines)
```

Update Task 1's `test_a_clean_entry_passes` summary expectation to the new format (`..., 0 bad filters, 0 bad entries`).

- [ ] **Step 2: Run to verify they fail**

Run: `python3 -m unittest discover -s scripts -p 'test_*.py'`
Expected: the seven new tests and the updated summary test FAIL (DUP/SHADOW still emitted, no REDUNDANT/NOFILTER/NOOP/ALSO).

- [ ] **Step 3: Implement**

In `check`:
- Remove the SHADOW block entirely. Replace it with a comment where the DUP block was, stating the reason once: an anchor nested in a longer one occurs once (AMBIG guarantees it), so it mutates a sub-span of the same site; and different replacements of one anchor are different mutations of one line. Neither can make a verdict lie, so neither is reported.
- NOFILTER: where the loop currently does `if not filt: continue`, record the failure and print the line, then `continue`.
- NOOP: in the per-entry loop, `if anchor == replacement:` record and print.
- Group by `(file, anchor, replacement)` preserving entry order. Within a group, the first entry is the reference; each later entry whose filter equals an earlier one in the group is REDUNDANT (name the first entry with that filter); otherwise it is ALSO (name the group's first entry).
- Exit 1 if stale, ambiguous, bad filters or bad entries; otherwise 0.
- Summary lines exactly as in **Interfaces**. If no warnings, print no warnings line.

- [ ] **Step 4: Run to verify they pass**

Run: `python3 -m unittest discover -s scripts -p 'test_*.py' -v`
Expected: all pass.

- [ ] **Step 5: Run the gate on the real table — it must fail on exactly the one redundant entry**

Run: `zsh scripts/mutation-check.sh --anchors-only > "$SCRATCH/anchors-t2.txt"; echo "exit $?"; grep -v '^FILTER? ' "$SCRATCH/anchors-t2.txt"`
Expected: one `REDUNDANT ingest: a stale skip does not start the strip  <-- repeats ingest: the runner re-checks change detection at pop time: ...` line, three `ALSO` lines (colour readability floor, mdmenu disabled rows, upload echo multiset), exit 1. If anything else fails, stop and report it — the measurement said it would not.

- [ ] **Step 6: Re-aim the redundant entry at the contract its name claims**

Read `crates/geode-data/src/ingest/runner.rs` around the stale check (~lines 820-850). The contract (see the comment above the entry, and the code comment "Announce Started only after the stale check") is: a stale skip emits no `Started`. The entry's mutation must move the `Started` announcement to BEFORE the `if stale { ... continue; }` block, keeping the stale skip itself intact. Write the anchor as the block from `let stale = ...` through the `Started` send so it is unique, and the replacement as the same text with the send moved above the `if stale`. Find the covering test by name: `grep -n "fn .*stale.*\|fn .*strip.*\|fn .*started.*" crates/geode-data/src/ingest/runner.rs crates/geode-data/src/service.rs` — it asserts that a stale skip produces no `Started`/`LoadEnded` pair. If none exists, write one in the runner's tests module named `a_stale_skip_announces_nothing` (enqueue a file, load it, re-queue it unchanged, assert the event receiver gets no `DataEvent::LoadStarted`-equivalent — use the exact variant name the code emits) and use it.

Verify the entry is real, both ways:
```bash
zsh scripts/mutation-check.sh "ingest: a stale skip does not start the strip"
```
Expected: `caught    ingest: a stale skip does not start the strip`. Then apply the replacement by hand, run `cargo test -p geode-data --lib <filter>`, and confirm the failure is an ASSERTION (`test result: FAILED`, a panic message from the test), not a compile error; restore with `git checkout -- crates/geode-data/src/ingest/runner.rs`. Record both outputs in your report.

- [ ] **Step 7: Update prose**

- `scripts/mutation-check.sh` header (~lines 9-20): replace the `DUP and SHADOW ...` sentences with the new verdicts: REDUNDANT, NOFILTER and NOOP are errors; ALSO is information (a second test catching the same mutation). Keep the "source scan, not Cargo test discovery" sentence.
- The comment above the two `launch: blotter` entries (~line 21878) currently says `--anchors-only` reports DUP; rewrite it to say the two entries share an anchor on purpose, mutating different behaviours of one line.
- `CLAUDE.md`: `zsh scripts/mutation-check.sh --anchors-only      # validate anchors, filters and entries, no Cargo`.
- `docs/current/data-path.md` harness paragraph (~line 521): say what `--anchors-only` now rejects (stale or ambiguous anchors, a filter matching no test or several without an exact name, an entry with no filter, a no-op replacement, a repeated mutation under the same test) and that the checker has its own unittest suite.

- [ ] **Step 8: Gate and commit**

```bash
python3 -m unittest discover -s scripts -p 'test_*.py'
zsh scripts/mutation-check.sh --anchors-only | tail -3; echo "exit $?"
cargo test -p geode-data --lib   # only if Step 6 added a test
git add -A scripts CLAUDE.md docs/current/data-path.md crates/geode-data
git commit -m "fix(scripts): fail on redundant, filterless and no-op entries

A shared anchor with a different replacement is a different mutation of
one line, and an anchor nested in a longer one is a sub-span of the same
site (AMBIG already guarantees one occurrence); neither can make a verdict
lie, so DUP and SHADOW are gone. What can lie is an entry repeating
another's mutation and test, an entry with no filter, or a replacement
equal to its anchor. The one redundant entry is re-aimed at the contract
its name claims: a stale skip announces no Started.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: A mutation that does not compile is not a catch

**Files:**
- Modify: `scripts/mutation-check.sh` (argument parsing ~lines 108-129; `run_mutation` ~lines 143-230; end-of-script summary; header comments ~lines 7-43 and ~84-107)
- Modify: `CLAUDE.md` (commands block), `docs/current/data-path.md` (harness paragraph)

**Interfaces:**
- Produces:
  - `BUILD     {name}  <-- mutation does not compile; no test ran` — printed by a mutation run when cargo fails with the compile-failure signature; the script exits 1 at the end if any BUILD was printed.
  - `--build-check` flag, in the first flag slot (mutually exclusive with `--anchors-only`): applies each selected mutation, runs `cargo check -p "$pkg" --tests`, prints `BUILD ...` on failure and nothing on success, restores the file, and ends with `build-checked N mutations: B do not compile`; exit 1 if B > 0. Composes with `--changed` and a substring exactly as a mutation run does.

- [ ] **Step 1: Implement the verdict**

In `run_mutation`, every place that currently echoes `caught` (both the filtered-failure branch and the unfiltered full-suite branch) and the `caught*` branch first classifies the log:

```zsh
# A mutation that does not compile fails cargo before any test runs.
# Reading that exit as a catch reports success for an entry that defends
# nothing, forever: the anchor still matches, so no static gate sees it.
compile_failed() {
  grep -q "could not compile" "$log"
}
```

and, where it would print `caught`/`caught*`:
```zsh
      if compile_failed; then
        echo "BUILD     $name  <-- mutation does not compile; no test ran"
        build_failures=$((build_failures + 1))
      else
        echo "caught    $name"
      fi
```
Declare `build_failures=0` beside `skipped=0`. At the end of the script (after the `--changed` summary line, before the anchors-only block), for mutation runs and build checks: `if (( build_failures )); then exit 1; fi`.

- [ ] **Step 2: Prove BUILD fires on a broken mutation and not on a real catch**

Add, temporarily and uncommitted, at the end of the entry table:
```zsh
run_mutation "zz probe: broken replacement" \
  crates/geode-chart/src/lib.rs \
  '<a unique line from that file — pick one with grep>' \
  'this does not compile' \
  geode-chart <any test name in geode-chart>
```
Run `zsh scripts/mutation-check.sh "zz probe"; echo "exit $?"` → expect `BUILD     zz probe: broken replacement ...` and `exit 1`. Run a known-good entry, e.g. `zsh scripts/mutation-check.sh "discovery: a changed file is reloaded"; echo "exit $?"` → expect `caught ...` and `exit 0`. Remove the probe entry. `git status` must show the chart file unmodified.

- [ ] **Step 3: Implement `--build-check`**

Parse it in the first flag slot beside `--anchors-only` (`build_only=1`). A second mode flag in the substring slot still hits the existing "unexpected argument" exit 2. In `run_mutation`, after the anchor-count checks and the mutation write, when `build_only`:

```zsh
  if (( build_only )); then
    built=$((built + 1))
    if ! cargo check -p "$pkg" --tests >"$log" 2>&1; then
      echo "BUILD     $name  <-- mutation does not compile; no test ran"
      build_failures=$((build_failures + 1))
    fi
    restore
    return 0
  fi
```
(`--tests` checks the library and binary unit-test targets under `cfg(test)`, which is where the mutated code and its tests compile together; it is the same code `cargo test` builds, without linking or running.) At the end print `build-checked $built mutations: $build_failures do not compile` when `build_only`.

- [ ] **Step 4: Update the header and docs**

- Usage line: `zsh scripts/mutation-check.sh [--anchors-only | --build-check] [--changed[=REF]] [substring]` (also the two `usage:` echo lines).
- Replace the header paragraph beginning "A failed Cargo command is reported as caught" with: a compile failure is reported as BUILD, is an error, and makes the run exit 1; `--build-check` compiles each selected mutation without running tests — an audit for replacements left stale by signature changes, which `--anchors-only` cannot see because it never compiles anything. Keep the sentence about deterministic fixtures.
- Update the verdict table comment (~lines 84-107) with the BUILD row.
- `CLAUDE.md` commands block: add `zsh scripts/mutation-check.sh --build-check "name substring"  # compile mutations, no tests`.
- `docs/current/data-path.md` harness paragraph: one sentence on BUILD and `--build-check`.

- [ ] **Step 5: Verify restore and composition**

```bash
zsh scripts/mutation-check.sh --build-check "discovery:"; echo "exit $?"; git status --short
zsh scripts/mutation-check.sh --build-check --anchors-only; echo "exit $?"
```
Expected: first run prints `build-checked N mutations: 0 do not compile` (N = the number of entries whose name contains `discovery:`), exit 0, and `git status` shows no modified source. Second prints the usage error, exit 2. Re-add the Step 2 probe, run `zsh scripts/mutation-check.sh --build-check "zz probe"` → BUILD line, exit 1, file restored; remove the probe.

- [ ] **Step 6: Commit**

```bash
git add scripts/mutation-check.sh CLAUDE.md docs/current/data-path.md
git commit -m "feat(scripts): a mutation that does not compile is BUILD, not caught

cargo failing at compile time read as a catch, so an entry whose
replacement went stale after a signature change reported success forever
while its anchor still matched. Mutation runs now say BUILD and exit 1;
--build-check compiles each selected mutation without running tests.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Small deferred items

**Files:**
- Modify: `crates/geode-data/src/egress.rs` (tests module: `GateAdapter` ~line 555, `PanicAdapter` ~line 657)
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: One adapter fixture.** `GateAdapter` and `PanicAdapter` differ only in the name they return. Replace both with
```rust
    /// Hands out one egress transport, once, under the given adapter name.
    struct TakeOnceAdapter {
        name: &'static str,
        egress: Mutex<Option<Box<dyn Egress>>>,
    }
```
whose `name()` returns `self.name`, and update every construction (`name: "gate"` / `name: "panic"`). Check `git grep -n "GateAdapter\|PanicAdapter" crates/` and the harness (`grep -n "GateAdapter\|PanicAdapter" scripts/mutation-check.sh`) — if an anchor contains either name, update the anchor and replacement text in the same commit and re-run `--anchors-only`.

Run: `cargo test -p geode-data --lib egress` → all pass.

- [ ] **Step 2: The static gate runs first in CI.** Move the `Mutation anchors` step (with its `if: runner.os != 'Windows'`) to directly after the `Mutation checker tests` step added in Task 1, i.e. immediately after checkout and before the toolchain install. It needs no toolchain; a stale anchor then fails in seconds instead of after the 40-minute test run.

- [ ] **Step 3: Commit**

```bash
cargo fmt --check && cargo clippy -p geode-data --all-targets -- -D warnings
zsh scripts/mutation-check.sh --anchors-only | tail -3
git add crates/geode-data/src/egress.rs .github/workflows/ci.yml scripts/mutation-check.sh
git commit -m "chore: one take-once adapter fixture; the static gate runs first in CI

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5 (controller): The build audit

Run by the controller, detached, not by a subagent:
```bash
nohup zsh scripts/mutation-check.sh --build-check > "$SCRATCH/build-audit.log" 2>&1 &
```
Wait with a bounded loop on `pgrep -f 'mutation-che[c]k'`. For every `BUILD` line: repair the entry's replacement so it compiles and still breaks the contract its name claims, and verify it the same two ways as Task 2 Step 6 (named run says `caught`; a by-hand application fails on an assertion). If the count is large (more than ~15), record the list in the branch handoff and ask Matthew before repairing them all in this branch.

Final gate before merge: `python3 -m unittest discover -s scripts -p 'test_*.py'`, `zsh scripts/mutation-check.sh --anchors-only` exit 0, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
