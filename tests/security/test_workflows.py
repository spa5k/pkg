"""Structural checks for PR-31 security and nightly workflow invariants."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
SECURITY = (ROOT / ".github/workflows/security.yml").read_text(encoding="utf-8")
NIGHTLY = (ROOT / ".github/workflows/nightly.yml").read_text(encoding="utf-8")
PINNED_USE = re.compile(r"^\s*uses:\s*actions/checkout@[0-9a-f]{40}\s*$", re.MULTILINE)


class WorkflowContractTests(unittest.TestCase):
    """Security evidence remains hermetic, pinned, and visibly scoped."""

    def test_security_lane_blocks_both_egress_families_before_offline_tests(self) -> None:
        block = SECURITY.index("Disable external IPv4 and IPv6 egress")
        execute = SECURITY.index("Run AC-S1 through AC-S10")
        self.assertLess(block, execute)
        self.assertIn("iptables -A OUTPUT -j REJECT", SECURITY)
        self.assertIn("ip6tables -A OUTPUT -j REJECT", SECURITY)
        self.assertIn("CARGO_NET_OFFLINE", (ROOT / "tests/security/run.py").read_text())

    def test_security_lane_is_reusable_and_owns_dependency_gates(self) -> None:
        self.assertIn("workflow_call:", SECURITY)
        self.assertIn("cargo deny --locked check", SECURITY)
        self.assertIn("cargo audit", SECURITY)
        self.assertIn("PKG_SECURITY_EXTERNAL_GATES: passed", SECURITY)

    def test_nightly_calls_security_and_runs_fault_harness_on_linux_and_macos(self) -> None:
        self.assertIn("uses: ./.github/workflows/security.yml", NIGHTLY)
        self.assertIn("os: [ubuntu-latest, macos-15]", NIGHTLY)
        self.assertIn("cargo test --offline --locked -p pkg-testkit chaos::tests", NIGHTLY)
        self.assertIn("cargo test --offline --locked --workspace", NIGHTLY)

    def test_external_actions_are_immutable_checkout_pins_only(self) -> None:
        for workflow in (SECURITY, NIGHTLY):
            external_uses = [
                line
                for line in workflow.splitlines()
                if "uses:" in line and "./.github/workflows/" not in line
            ]
            self.assertTrue(external_uses)
            self.assertTrue(all(PINNED_USE.fullmatch(line) for line in external_uses))


if __name__ == "__main__":
    unittest.main()
