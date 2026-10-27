import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/checks/loc.py"
SPEC = importlib.util.spec_from_file_location("loc", SCRIPT)
LOC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LOC)


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.policy = {"default_file_lines": 2, "components": {"src/": 3}}
        (self.root / "src").mkdir()

    def write(self, name, text):
        (self.root / name).write_text(text)

    def test_file_at_limit_and_above(self):
        self.write("src/a.rs", "one\ntwo\n")
        self.assertEqual(LOC.check(self.root, self.policy, ["src/a.rs"])[1], [])
        self.write("src/a.rs", "one\ntwo\nthree\n")
        failures = LOC.check(self.root, self.policy, ["src/a.rs"])[1]
        self.assertEqual(len(failures), 1)
        self.assertIn("exceeds 2", failures[0])

    def test_component_total_and_duplicate_paths(self):
        self.write("src/a.rs", "one\ntwo\n")
        self.write("src/b.rs", "one\ntwo\n")
        counts, failures = LOC.check(self.root, self.policy, ["src/a.rs", "src/a.rs", "src/b.rs"])
        self.assertEqual(sum(counts.values()), 4)
        self.assertIn("component budget", failures[0])

    def test_exception_requires_ownership(self):
        self.write("src/a.rs", "one\ntwo\nthree\n")
        self.policy["files"] = {"src/a.rs": {"limit": 3}}
        self.assertIn("lacks owner", LOC.check(self.root, self.policy, ["src/a.rs"])[1][0])
        self.policy["files"]["src/a.rs"].update(owner="maintainer", rationale="Reviewed baseline")
        self.assertEqual(LOC.check(self.root, self.policy, ["src/a.rs"])[1], [])

    def test_non_source_and_deleted_paths(self):
        self.write("diagram.svg", "one\ntwo\nthree\n")
        self.assertEqual(LOC.check(self.root, self.policy, ["diagram.svg", "missing.rs"]), ({}, []))

    def test_blank_lines_count(self):
        self.write("src/a.rs", "one\n\nthree\n")
        self.assertEqual(LOC.check(self.root, self.policy, ["src/a.rs"])[0]["src/a.rs"], 3)

    def test_new_component_needs_a_budget(self):
        self.write("new.py", "one\n")
        self.assertIn("no component budget", LOC.check(self.root, self.policy, ["new.py"])[1][0])


if __name__ == "__main__":
    unittest.main()
