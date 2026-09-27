"""Static checks behind `mutation-check.sh --anchors-only`.

Reads the NUL-separated six-field records the zsh script collects (name,
file, anchor, replacement, package, filter) and checks every anchor for
exactly one match in its file and every test filter against the
test-attributed function names under the package's src directory. It runs no
Cargo command and edits no file.

Missing or ambiguous anchors, invalid filters, an entry with no filter, a
replacement equal to its anchor, a mutation repeated under the same test, and
an empty selection fail. Loose filters and a mutation repeated under another
test are information.
"""

import collections
import pathlib
import re
import sys

Entry = collections.namedtuple("Entry", "name file anchor replacement pkg filter")

FN_DECL = re.compile(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")
# Anchored at `test` after an optional path so `#[cfg(any(test, ...))]` and
# `#[cfg_attr(not(test), ...)]` do not mark the next function as a test.
TEST_ATTR = re.compile(r"^#\[(?:\w+::)*test(\]|\()")

FIELDS = len(Entry._fields)


def read_entries(raw):
    """Split NUL-terminated fields into six-field entries."""
    fields = raw.split(b"\0")[:-1] if raw else []
    return [
        Entry(*(f.decode() for f in fields[i:i + FIELDS]))
        for i in range(0, len(fields), FIELDS)
    ]


def test_fns(root, pkg):
    """Names of test-attributed functions in a package.

    A filter is what cargo is handed, and cargo matches a substring against
    the test's path. Matching against every `fn` would let a filter naming a
    plain helper pass, so only functions carrying a `test` attribute count.
    """
    names = set()
    for path in sorted((root / "crates" / pkg / "src").rglob("*.rs")):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError):
            # A file that cannot be read declares no test a filter could
            # name; skipping it can only fail a filter, never pass one.
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
                if TEST_ATTR.match(stripped):
                    saw_test_attr = True
            elif stripped and not stripped.startswith("//"):
                saw_test_attr = False
    return names


def check(entries, root):
    """Return (output lines, exit code) for the selected entries."""
    if not entries:
        return ["checked 0 anchors (nothing selected)"], 1

    out = []
    fn_cache = {}
    texts = {}
    stale = ambiguous = bad_filters = bad_entries = loose = 0
    for e in entries:
        name, file, anchor, pkg, filt = e.name, e.file, e.anchor, e.pkg, e.filter
        if file not in texts:
            try:
                texts[file] = (root / file).read_text(encoding="utf-8")
            except OSError:
                texts[file] = None
            except UnicodeDecodeError:
                texts[file] = False
        text = texts[file]
        if text is None:
            stale += 1
            out.append(f"ANCHOR    {name}  <-- file missing: {file}")
            continue
        if text is False:
            # An undecodable file cannot be searched, so the anchor cannot be
            # shown to exist; reading it as absent keeps the gate failing.
            stale += 1
            out.append(f"ANCHOR    {name}  <-- could not read {file} as UTF-8")
            continue
        hits = text.count(anchor)
        if hits == 0:
            stale += 1
            out.append(f"ANCHOR    {name}  <-- anchor no longer matches; mutation is stale")
        elif hits > 1:
            ambiguous += 1
            out.append(f"AMBIG x{hits}  {name}  <-- anchor matches {hits} times; only the first is mutated")
        if anchor == e.replacement:
            # The harness would run the tests on unchanged source and report
            # the survivor as a missed mutation of code it never touched.
            bad_entries += 1
            out.append(f"NOOP      {name}  <-- replacement equals the anchor; nothing is mutated")
        if not filt:
            # Without a filter the whole package runs, so "caught" names no
            # test and cannot show which contract the entry guards.
            bad_entries += 1
            out.append(f"NOFILTER  {name}  <-- names no detecting test")
            continue
        if pkg not in fn_cache:
            fn_cache[pkg] = test_fns(root, pkg)
        matched = sorted(n for n in fn_cache[pkg] if filt in n)
        if not matched:
            bad_filters += 1
            out.append(f"FILTER    {name}  <-- '{filt}' matches no test in {pkg}")
        elif len(matched) > 1 and filt not in matched:
            # Several function names match the substring but none is the exact
            # requested name; require an unambiguous detecting-test declaration.
            bad_filters += 1
            out.append(f"FILTERx {len(matched)}  {name}  <-- '{filt}' matches {len(matched)} tests, none of them exactly")
        elif len(matched) > 1:
            # The named test does run; the siblings only make the entry slower and
            # make "which test caught it" unanswerable.
            loose += 1
            out.append(f"FILTER? {len(matched)}  {name}  <-- '{filt}' also matches {len(matched) - 1} sibling test(s)")

    # An anchor nested in a longer one occurs once (AMBIG guarantees it), so it
    # mutates a sub-span of the same site; and different replacements of one
    # anchor are different mutations of one line. Neither can make a verdict
    # lie, so neither is reported. What can is the same mutation twice: under
    # the same test (package and filter) it only repeats a verdict (REDUNDANT,
    # an error); under another test it is a second detector of one mutation
    # (ALSO, information).
    groups = collections.defaultdict(list)
    for e in entries:
        if e.filter:
            groups[(e.file, e.anchor, e.replacement)].append(e)
    also = 0
    for group in groups.values():
        # Keyed on (package, filter): one test-fn name in two packages names
        # two different tests, so the second entry is a second detector, not
        # a repeated verdict.
        first_by_test = {}
        for e in group:
            test = (e.pkg, e.filter)
            if test in first_by_test:
                bad_entries += 1
                out.append(
                    f"REDUNDANT {e.name}  <-- repeats {first_by_test[test].name}: "
                    "same anchor, replacement and test"
                )
                continue
            if first_by_test:
                also += 1
                out.append(f"ALSO      {e.name}  <-- same mutation as {group[0].name}, detected by '{e.filter}'")
            first_by_test[test] = e

    out.append(
        f"checked {len(entries)} anchors: {stale} stale, {ambiguous} ambiguous, "
        f"{bad_filters} bad filters, {bad_entries} bad entries"
    )
    warnings = []
    if loose:
        warnings.append(f"{loose} loose filters")
    if also:
        warnings.append(f"{also} same-mutation entries")
    if warnings:
        out.append(f"  {', '.join(warnings)}")
    return out, (1 if stale or ambiguous or bad_filters or bad_entries else 0)


def main(argv):
    """`python3 scripts/mutation_anchors.py <record-file>`, run from the repo top.

    A missing record file reads as an empty selection, which fails.
    """
    path = pathlib.Path(argv[1])
    raw = path.read_bytes() if path.exists() else b""
    lines, code = check(read_entries(raw), pathlib.Path("."))
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
