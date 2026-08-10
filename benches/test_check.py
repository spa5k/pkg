"""Contract tests for the closed G-PERF baseline gate."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from check import GateError, evaluate

ROOT = Path(__file__).resolve().parents[1]


class PerformanceGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.baseline = self.root / "baseline.json"
        self.criterion = self.root / "criterion"
        self.document = {
            "schemaVersion": 1,
            "platform": "test-native",
            "maxRegressionPercent": 25,
            "provenance": {
                "evidenceClass": "native",
                "runner": "test",
                "rust": "1.96.1",
                "collectedAt": "2026-08-10",
                "command": "cargo bench",
            },
            "benchmarks": {
                name: {"baselineNs": 100, "absoluteCeilingNs": 1000}
                for name in (
                    "index_build_tiny",
                    "search_ripgrep",
                    "info_requests",
                )
            },
        }

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write(self, measured: float = 100) -> None:
        self.baseline.write_text(json.dumps(self.document), encoding="utf-8")
        for name in self.document["benchmarks"]:
            result = self.criterion / name / "new"
            result.mkdir(parents=True, exist_ok=True)
            (result / "estimates.json").write_text(
                json.dumps({"median": {"point_estimate": measured}}), encoding="utf-8"
            )

    def test_accepts_complete_result_below_both_limits(self) -> None:
        self.write(125)
        self.assertEqual(
            len(evaluate(self.baseline, self.criterion, "test-native", "test")), 3
        )

    def test_rejects_regression_even_below_absolute_ceiling(self) -> None:
        self.write(126)
        with self.assertRaisesRegex(GateError, "regressed by more than 25%"):
            evaluate(self.baseline, self.criterion, "test-native", "test")

    def test_rejects_absolute_failure_even_with_loose_baseline(self) -> None:
        self.document["benchmarks"]["search_ripgrep"] = {
            "baselineNs": 900,
            "absoluteCeilingNs": 1000,
        }
        self.write(1100)
        with self.assertRaisesRegex(GateError, "exceeds its absolute ceiling"):
            evaluate(self.baseline, self.criterion, "test-native", "test")

    def test_missing_data_fails_closed(self) -> None:
        del self.document["benchmarks"]["info_requests"]
        self.write()
        with self.assertRaisesRegex(GateError, "exactly match"):
            evaluate(self.baseline, self.criterion, "test-native", "test")

    def test_rejects_non_native_or_wrong_platform_baseline(self) -> None:
        self.document["provenance"]["evidenceClass"] = "qemu"
        self.write()
        with self.assertRaisesRegex(GateError, "native evidence"):
            evaluate(self.baseline, self.criterion, "test-native", "test")
        self.document["provenance"]["evidenceClass"] = "native"
        self.write()
        with self.assertRaisesRegex(GateError, "does not match"):
            evaluate(self.baseline, self.criterion, "other-platform", "test")

    def test_rejects_a_different_runner_of_the_same_platform(self) -> None:
        self.write()
        with self.assertRaisesRegex(GateError, "baseline runner"):
            evaluate(self.baseline, self.criterion, "test-native", "other-runner")


class PerformanceWorkflowTests(unittest.TestCase):
    def test_workflow_is_pinned_to_the_baseline_reference_host(self) -> None:
        workflow = (ROOT / ".github/workflows/performance.yml").read_text(encoding="utf-8")
        self.assertIn("runs-on: [self-hosted, pkg-perf-reference, macOS, ARM64]", workflow)
        self.assertEqual(workflow.count("PKG_PERF_RUNNER: ${{ runner.name }}"), 2)
        self.assertNotIn("PKG_PERF_RUNNER: pkg-perf-reference-m4", workflow)
        self.assertIn("test \"$(uname -m)\" = arm64", workflow)
        self.assertNotIn("ubuntu-latest", workflow)
        self.assertNotIn("pull_request:", workflow)
        self.assertIn("push:\n    branches: [main]", workflow)
        self.assertIn("if: github.ref == 'refs/heads/main'", workflow)
        self.assertIn("ref: main", workflow)

    def test_workflow_pins_external_inputs_and_gates_both_native_outputs(self) -> None:
        workflow = (ROOT / ".github/workflows/performance.yml").read_text(encoding="utf-8")
        self.assertIn("actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd", workflow)
        self.assertIn(
            "rust@sha256:1f0dbad1df66647807e6952d1db85d0b2bda7606cb2139d82517e4f009967376",
            workflow,
        )
        self.assertIn("--platform aarch64-darwin", workflow)
        self.assertIn("--platform aarch64-linux", workflow)
        self.assertIn("cargo bench --locked -p pkg-index --bench v1", workflow)


if __name__ == "__main__":
    unittest.main()
