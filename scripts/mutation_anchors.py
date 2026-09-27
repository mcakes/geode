"""Static checks behind `mutation-check.sh --anchors-only`.

Reads the NUL-separated six-field records the zsh script collects (name,
file, anchor, replacement, package, filter) and checks every anchor for
exactly one match in its file and every nonempty test filter against the
test-attributed function names under the package's src directory. It runs no
Cargo command and edits no file.

Missing or ambiguous anchors, invalid filters, and an empty selection fail;
loose filters and shared or overlapping anchors are warnings.
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
    stale = ambiguous = bad_filters = loose = 0
    for e in entries:
        name, file, anchor, pkg, filt = e.name, e.file, e.anchor, e.pkg, e.filter
        if file not in texts:
            try:
                texts[file] = (root / file).read_text(encoding="utf-8")
            except OSError:
                texts[file] = None
        text = texts[file]
        if text is None:
            stale += 1
            out.append(f"ANCHOR    {name}  <-- file missing: {file}")
            continue
        hits = text.count(anchor)
        if hits == 0:
            stale += 1
            out.append(f"ANCHOR    {name}  <-- anchor no longer matches; mutation is stale")
        elif hits > 1:
            ambiguous += 1
            out.append(f"AMBIG x{hits}  {name}  <-- anchor matches {hits} times; only the first is mutated")
        if not filt:
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

    # Shared source anchors may carry different replacements. Report the overlap
    # for review without treating it as proof that the mutations are redundant.
    by_anchor = collections.defaultdict(list)
    for e in entries:
        by_anchor[(e.file, e.anchor)].append(e.name)
    dup_groups = {k: v for k, v in by_anchor.items() if len(v) > 1}
    dup_entries = sum(len(v) for v in dup_groups.values())
    for (file, _anchor), names in sorted(dup_groups.items()):
        for shadowed_name in names[1:]:
            out.append(f"DUP       {shadowed_name}  <-- shares (file, anchor) with {names[0]}")

    # An anchor that occurs once but sits inside a longer anchor another entry
    # uses. AMBIG counts occurrences of one anchor and cannot see this.
    anchors_by_file = collections.defaultdict(set)
    for e in entries:
        anchors_by_file[e.file].add(e.anchor)
    shadowed_anchors = sorted(
        (file, anchor)
        for file, anchors in anchors_by_file.items()
        for anchor in anchors
        if any(other != anchor and anchor in other for other in anchors)
    )
    example_of = {}
    for e in entries:
        example_of.setdefault((e.file, e.anchor), e.name)
    for file, anchor in shadowed_anchors:
        out.append(f"SHADOW    {example_of[(file, anchor)]}  <-- anchor is a substring of a longer anchor in {file}")

    out.append(f"checked {len(entries)} anchors: {stale} stale, {ambiguous} ambiguous, {bad_filters} bad filters")
    warnings = []
    if loose:
        warnings.append(f"{loose} loose filters")
    if dup_entries:
        warnings.append(f"{dup_entries} duplicate anchors in {len(dup_groups)} groups")
    if shadowed_anchors:
        warnings.append(f"{len(shadowed_anchors)} shadowed anchors")
    if warnings:
        out.append(f"  {', '.join(warnings)}")
        out.append("  (warnings; see the anchor-uniqueness follow-up)")
    # Shared and overlapping anchors are advisory; only stale/ambiguous locations
    # and invalid test filters fail this check.
    return out, (1 if stale or ambiguous or bad_filters else 0)


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
