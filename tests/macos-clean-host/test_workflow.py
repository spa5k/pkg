"""Structural safety contract for the manual macOS preview proof."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/macos-alpha-proof.yml").read_text()
NIGHTLY = (ROOT / ".github/workflows/nightly.yml").read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()


class MacOsProofWorkflowTests(unittest.TestCase):
    def test_manual_read_only_proof_uses_a_source_free_second_host(self) -> None:
        self.assertIn("workflow_dispatch:", WORKFLOW)
        self.assertIn("workflow_call:", WORKFLOW)
        self.assertIn("permissions:\n  contents: read", WORKFLOW)
        self.assertNotIn("secrets.", WORKFLOW)
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1]
        self.assertNotIn("actions/checkout", proof_job)
        self.assertNotIn("cargo ", proof_job)
        self.assertIn('PKG_DISPOSABLE_MACOS_PROOF: confirmed', proof_job)

    def test_default_branch_workflow_can_dispatch_the_branch_proof(self) -> None:
        self.assertIn("macos_alpha_proof:", NIGHTLY)
        self.assertIn("if: ${{ inputs.macos_alpha_proof }}", NIGHTLY)
        self.assertIn("uses: ./.github/workflows/macos-alpha-proof.yml", NIGHTLY)
        self.assertEqual(
            NIGHTLY.count(
                "if: ${{ github.event_name != 'workflow_dispatch' "
                "|| !inputs.macos_alpha_proof }}"
            ),
            3,
        )

    def test_third_party_actions_are_commit_pinned(self) -> None:
        for line in WORKFLOW.splitlines():
            if "uses: actions/" in line:
                revision = line.rsplit("@", 1)[-1]
                self.assertEqual(len(revision), 40)
                int(revision, 16)

    def test_local_tart_gate_requires_kernel_vm_identity(self) -> None:
        local_gate = PROOF.split("    local-tart)\n", 1)[1].split("        ;;\n", 1)[0]
        self.assertIn('"$(/usr/bin/uname -m)" = arm64', local_gate)
        self.assertIn("kern.hv_vmm_present", local_gate)
        self.assertIn("VirtualMac*", local_gate)
        self.assertIn("root:wheel:600", PROOF)
        self.assertIn('"$marker_age" -le 300', PROOF)
        self.assertIn('${GITHUB_ACTIONS:-}" = true', PROOF)
        self.assertIn('${RUNNER_ENVIRONMENT:-}" = github-hosted', PROOF)
        self.assertIn('*) fail "the disposable-host gate is absent"', PROOF)
        self.assertLess(PROOF.index("case \"${PKG_DISPOSABLE_MACOS_PROOF:-}\""), PROOF.index("bundle="))

    def test_local_tart_gate_waits_for_apfs_resize(self) -> None:
        self.assertIn('echo "+ wait for stable root APFS container"', PROOF)
        self.assertIn("APFSContainerSize", PROOF)
        self.assertIn("/usr/bin/pgrep -x diskutil", PROOF)
        self.assertIn('"$stable_apfs_samples" -ge 3', PROOF)
        self.assertLess(
            PROOF.index("+ wait for stable root APFS container"),
            PROOF.index("security add-trusted-cert"),
        )

    def test_apfs_checkpoint_parses_the_nested_journal(self) -> None:
        self.assertIn('e.get("mutation", {}).get("kind") == "storeVolume"', PROOF)
        self.assertNotIn('"kind":"storeVolume","state":"created"', PROOF)

    def test_product_volume_disables_spotlight_and_preserves_uninstall_failure(self) -> None:
        self.assertIn("/usr/bin/mdutil -s /nix", PROOF)
        self.assertIn('/bin/cat "$work/interrupted-uninstall.log" >&2', PROOF)
        self.assertIn("/usr/bin/sudo /bin/test -f", PROOF)
        self.assertNotIn("/usr/bin/sudo /usr/bin/test -f", PROOF)

    def test_transient_apfs_refusal_gets_one_exact_public_retry(self) -> None:
        uninstall = PROOF.split('echo "+ interrupt uninstall after APFS removal"', 1)[1]
        uninstall = uninstall.split('echo "+ recover uninstall', 1)[0]
        self.assertEqual(uninstall.count("+ exact retry after transient APFS refusal"), 1)
        self.assertIn('"$work/pkg-after-uninstall" --yes uninstall', uninstall)
        self.assertIn("product_volume_present", uninstall)
        self.assertIn("macos-transaction-v1.json", uninstall)
        self.assertIn('while [ "$attempt" -lt 1800 ]', uninstall)
        self.assertIn("/bin/sleep 1", uninstall)
        self.assertNotIn("/bin/sleep 0.05", uninstall)


if __name__ == "__main__":
    unittest.main()
