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
        self.assertIn("checked 1 anchors: 0 stale, 0 ambiguous, 0 bad filters", lines)

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
