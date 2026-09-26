"""Exercise the mutation gate with real Python owners and subprocess test runs."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

from check_test_contracts import check_case


class ContractGateTests(unittest.TestCase):
    def run_fixture(self, body, mutation="return 8", owner="return 7"):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            source = work / "owner.py"
            source.write_text(f"def value():\n    {owner}\n")
            original = source.read_bytes()
            (work / "test_owner.py").write_text(
                "import unittest\nfrom owner import value\n"
                "class OwnerTest(unittest.TestCase):\n" + body)
            command = [sys.executable, "-B", "-m", "unittest", "test_owner", "-v"]
            case = ("fixture", "owner.py", owner, mutation, command)
            try:
                return check_case(case, work, os.environ, work)
            finally:
                self.assertEqual(source.read_bytes(), original)

    def test_real_behavior_failure_is_caught_and_source_is_restored(self):
        result = self.run_fixture("    def test_value(self):\n        self.assertEqual(value(), 7)\n")
        self.assertTrue(result["passed"])
        self.assertTrue(result["mutant"]["assertion_failed"])
        self.assertTrue(result["restored"]["healthy"])

    def test_weakened_assertion_leaves_the_mutant_alive(self):
        result = self.run_fixture("    def test_value(self):\n        self.assertIsInstance(value(), int)\n")
        self.assertFalse(result["passed"])

    def test_missing_or_skipped_tests_fail_the_baseline(self):
        for body in ["    pass\n", "    @unittest.skip('unavailable')\n    def test_value(self):\n        self.fail()\n"]:
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, "baseline"):
                self.run_fixture(body)

    def test_import_error_is_not_a_caught_mutation(self):
        result = self.run_fixture("    def test_value(self):\n        self.assertEqual(value(), 7)\n",
                                  mutation="import missing_pkg_contract_fixture_dependency; return 8")
        self.assertFalse(result["passed"])
        self.assertFalse(result["mutant"]["assertion_failed"])

    def test_timeout_status_after_assertion_summary_is_not_a_caught_mutation(self):
        result = self.run_fixture("    def test_value(self):\n        self.assertEqual(value(), 7)\n",
                                  mutation="import atexit, os; atexit.register(lambda: os._exit(124)); return 8")
        self.assertEqual(result["mutant"]["exit_code"], 124)
        self.assertFalse(result["passed"])
        self.assertTrue(result["restored"]["healthy"])

    def run_child_fixture(self, mutation):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            source = work / "owner.py"
            source.write_text("print(7)\n")
            (work / "test_owner.py").write_text(
                "import subprocess, sys, unittest\n"
                "class OwnerTest(unittest.TestCase):\n"
                "    def test_value(self):\n"
                "        child = subprocess.run([sys.executable, 'owner.py'], capture_output=True, text=True)\n"
                "        self.assertEqual(child.returncode, 0, child.stderr)\n"
                "        self.assertEqual(child.stdout.strip(), '7')\n")
            case = ("fixture", "owner.py", "print(7)", mutation,
                    [sys.executable, "-B", "-m", "unittest", "test_owner", "-v"])
            try:
                return check_case(case, work, os.environ, work)
            finally:
                self.assertEqual(source.read_text(), "print(7)\n")

    def test_invalid_child_source_cannot_hide_behind_an_exit_code_assertion(self):
        with self.assertRaisesRegex(ValueError, "invalid Python"):
            self.run_child_fixture("this is invalid Python !")

    def test_child_import_failure_is_not_a_caught_mutation(self):
        result = self.run_child_fixture("import missing_pkg_contract_fixture_dependency; print(7)")
        self.assertFalse(result["passed"])
        self.assertFalse(result["mutant"]["assertion_failed"])
        self.assertTrue(result["restored"]["healthy"])

    def test_changed_owner_requires_an_explicit_mutation_update(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            (work / "owner.py").write_text("def value():\n    return 7\n")
            case = ("fixture", "owner.py", "return 99", "return 8", [sys.executable])
            with self.assertRaisesRegex(ValueError, "target changed"):
                check_case(case, work, os.environ, work)
