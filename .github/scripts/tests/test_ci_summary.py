import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("summary", Path(__file__).resolve().parents[1] / "ci_summary.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SummaryTests(unittest.TestCase):
    def test_failed_cancelled_and_skipped_are_never_passes(self):
        for status in ("failure", "cancelled", "skipped", "unknown"):
            text, passed = module.summary("Results", {"proof": {"result": status}})
            self.assertFalse(passed)
            self.assertNotIn("Passed", text)

    def test_outcome_wins_over_continue_on_error_conclusion(self):
        _, passed = module.summary("Results", {"test": {"outcome": "failure", "conclusion": "success"}})
        self.assertFalse(passed)

    def test_cells_cannot_inject_table_rows_or_html(self):
        text, passed = module.summary("<b>Results</b>", {"x|\n<script>": {"result": "success"}})
        self.assertTrue(passed)
        self.assertNotIn("<script>", text)
        self.assertIn("&#124;", text)
