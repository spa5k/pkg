"""Focused checks for the prepare-capable macOS lifecycle harness."""

import os
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = Path(
    os.environ.get("PKG_MACOS_PROOF_WORKFLOW", ROOT / ".github/workflows/macos-alpha-proof.yml")
)
WORKFLOW = WORKFLOW_PATH.read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()


class MacOsPrepareWorkflowTests(unittest.TestCase):
    def test_dispatch_and_inputs_are_immutable(self) -> None:
        self.assertIn("workflow_dispatch:", WORKFLOW)
        self.assertIn("environment: release", WORKFLOW)
        self.assertIn('test "$GITHUB_WORKFLOW_SHA" = "$EXPECTED_SHA"', WORKFLOW)
        self.assertIn(
            "PKG_PROOF_PAIR_SHA256: "
            "2f8ef35f460c0e36357a3922d073c14931446cc639e27df96d5cb46a5308e609",
            WORKFLOW,
        )

    def test_hosted_acquisition_is_full_and_fail_closed(self) -> None:
        acquire = WORKFLOW.split("\n  acquire-inputs:\n", 1)[1].split("\n  prove:\n", 1)[0]
        for required in (
            "--proto '=https'",
            'test "$response" = "200 $url"',
            "proof inventory has missing or extra entries",
            'fetch "$PROOF_CHANNEL_URL/$name/$path"',
            "select(.isDraft == true)",
            "cosign verify-blob",
        ):
            self.assertIn(required, acquire)
        self.assertNotIn("--location", acquire)
        self.assertNotIn("assert ", acquire)

    def test_prepare_call_supplies_the_complete_phase_contract(self) -> None:
        prove = WORKFLOW.split("\n  prove:\n", 1)[1].split("\n  aggregate:\n", 1)[0]
        for required in (
            "PKG_PROOF_CHANNEL_URL: ${{ inputs.proof_channel_url }}",
            "PKG_PROOF_PAIR_SHA256: ${{ env.PKG_PROOF_PAIR_SHA256 }}",
            "PKG_PROOF_LIFECYCLE_RUN: ${{ matrix.lifecycle_run }}",
            "PKG_PROOF_PHASE: prepare",
            '"phase=prepare"',
        ):
            self.assertIn(required, prove)

    def test_prepare_aggregate_requires_real_results(self) -> None:
        aggregate = WORKFLOW.split("\n  aggregate:\n", 1)[1]
        for required in (
            'test "$PROVE_RESULT" = success',
            '"phase": "prepare"',
            '"status": "passed"',
            "runner\\tcontinuation-recorded\\tpass",
            "the two prepares used the same VM nonce",
        ):
            self.assertIn(required, aggregate)
        self.assertIn("python3 -I -", aggregate)

    def test_harness_has_two_explicit_phases_without_a_default(self) -> None:
        self.assertIn('case "$PKG_PROOF_PHASE" in prepare|resume)', PROOF)
        self.assertNotIn("PKG_PROOF_PHASE:-", PROOF)
        self.assertIn("PKG-DN16-CONTINUATION-V1", PROOF)
        self.assertIn('[ "$old_boot" != "$current_boot" ]', PROOF)

    def test_prepare_snapshots_before_the_n_plus_1_upgrade(self) -> None:
        install = PROOF.index('capture package-state-install "$pkg"')
        snapshot = PROOF.index("persist_prepare_state", install)
        upgrade = PROOF.index('capture staged-channel-upgrade /usr/bin/sudo "$to_installer"')
        self.assertLess(install, snapshot)
        self.assertLess(snapshot, upgrade)
        self.assertIn("compare_prepare_state", PROOF)
        self.assertIn("assert_services_offline", PROOF)

    def test_security_python_and_actions_are_pinned(self) -> None:
        self.assertNotIn("assert ", PROOF)
        self.assertIn("/usr/bin/python3 -I -", PROOF)
        for line in WORKFLOW.splitlines():
            if "uses:" in line and "./" not in line:
                revision = line.rsplit("@", 1)[-1]
                self.assertEqual(len(revision), 40)
                int(revision, 16)


if __name__ == "__main__":
    unittest.main()
