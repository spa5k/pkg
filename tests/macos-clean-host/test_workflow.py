"""Structural safety contract for the manual macOS preview proof."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/macos-alpha-proof.yml").read_text()


class MacOsProofWorkflowTests(unittest.TestCase):
    def test_manual_read_only_proof_uses_a_source_free_second_host(self) -> None:
        self.assertIn("workflow_dispatch:", WORKFLOW)
        self.assertIn("permissions:\n  contents: read", WORKFLOW)
        self.assertNotIn("secrets.", WORKFLOW)
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1]
        self.assertNotIn("actions/checkout", proof_job)
        self.assertNotIn("cargo ", proof_job)
        self.assertIn('PKG_DISPOSABLE_MACOS_PROOF: confirmed', proof_job)

    def test_third_party_actions_are_commit_pinned(self) -> None:
        for line in WORKFLOW.splitlines():
            if "uses: actions/" in line:
                revision = line.rsplit("@", 1)[-1]
                self.assertEqual(len(revision), 40)
                int(revision, 16)


if __name__ == "__main__":
    unittest.main()
