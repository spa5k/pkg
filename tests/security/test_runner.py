"""Unit tests for the closed security-lane manifest and runner."""

from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest


RUNNER_PATH = Path(__file__).with_name("run.py")
SPEC = importlib.util.spec_from_file_location("pkg_security_runner", RUNNER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("security runner module cannot be loaded")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class SecurityManifestTests(unittest.TestCase):
    """The lane is complete, ordered, and cannot widen into shell execution."""

    def test_committed_manifest_is_exact_and_closed(self) -> None:
        cases = RUNNER.load_manifest()
        self.assertEqual(tuple(case["id"] for case in cases), RUNNER.EXPECTED_IDS)
        self.assertEqual(cases[7]["externalGates"], ["cargo-deny", "cargo-audit"])

    def test_missing_duplicate_or_out_of_order_case_is_rejected(self) -> None:
        cases = RUNNER.load_manifest()
        for mutated in (cases[:-1], cases + [copy.deepcopy(cases[-1])], list(reversed(cases))):
            document = {"schemaVersion": 1, "cases": mutated}
            with self.assertRaises(RUNNER.ManifestError):
                RUNNER.validate_manifest(document)

    def test_shell_network_and_unapproved_manifests_are_rejected(self) -> None:
        invalid = [
            ["sh", "-c", "cargo test"],
            ["cargo", "test", "--locked", "pkg"],
            ["cargo", "test", "--offline", "--locked", "../outside"],
            [
                "cargo",
                "test",
                "--offline",
                "--locked",
                "--manifest-path",
                "other/Cargo.toml",
            ],
        ]
        for command in invalid:
            with self.assertRaises(RUNNER.ManifestError):
                RUNNER.validate_command(command)

    def test_malformed_external_gate_type_fails_closed(self) -> None:
        case = copy.deepcopy(RUNNER.load_manifest()[7])
        case["externalGates"] = [{"name": "cargo-audit"}]
        with self.assertRaises(RUNNER.ManifestError):
            RUNNER.validate_case(case)


if __name__ == "__main__":
    unittest.main()
