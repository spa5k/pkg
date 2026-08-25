"""Structural release-workflow security contract."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
PUBLISH_WORKFLOW = (ROOT / ".github/workflows/publish-release.yml").read_text(
    encoding="utf-8"
)
LINUX_HARNESS = (ROOT / "tests/linux-clean-host/run.sh").read_text(encoding="utf-8")
MACOS_WORKFLOW = (ROOT / ".github/workflows/macos-alpha-proof.yml").read_text(
    encoding="utf-8"
)


class ReleaseWorkflowTests(unittest.TestCase):
    def test_dry_run_has_read_only_permissions_and_pinned_checkout(self) -> None:
        self.assertIn("permissions:\n  contents: read", WORKFLOW)
        self.assertIn(
            "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd",
            WORKFLOW,
        )
        self.assertIn("persist-credentials: false", WORKFLOW)
        self.assertIn("cargo test --locked -p pkg-release", WORKFLOW)
        self.assertIn(
            "python3 -m unittest discover -s tools/release -p 'test_*.py' -v",
            WORKFLOW,
        )

    def test_workflow_never_loads_a_production_key_or_publishes(self) -> None:
        self.assertNotIn("secrets.", WORKFLOW)
        self.assertNotIn("contents: write", WORKFLOW)
        self.assertNotIn("gh release", WORKFLOW)
        self.assertNotIn("aws-actions", WORKFLOW)
        self.assertIn("in-memory Ed25519 test keys", (ROOT / "tools/release/README.md").read_text())

    def test_linux_alpha_artifact_is_retained_but_not_published(self) -> None:
        self.assertIn('- "crates/**"', WORKFLOW)
        self.assertIn("tests/linux-clean-host/run.sh --keep-artifacts", WORKFLOW)
        self.assertIn(
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            WORKFLOW,
        )
        self.assertIn("PKG_CARGO_ABOUT:", WORKFLOW)
        self.assertIn("cargo-about --version 0.9.1", WORKFLOW)
        self.assertIn("cargo fetch --locked", WORKFLOW)
        self.assertNotIn("PKG_NIX_SOURCE_ARCHIVE:", WORKFLOW)
        self.assertNotIn("nix-2.34.8", WORKFLOW)
        candidate = "pkg-v0.1.0-alpha.7-linux-x86_64.tar.gz"
        self.assertIn(f"proof-artifacts/{candidate}", WORKFLOW)
        self.assertIn('"pkg-${RELEASE_TAG}-linux-x86_64.tar.gz"', PUBLISH_WORKFLOW)
        self.assertIn("pkg-v0.1.0-alpha.7-linux-x86_64-candidate", WORKFLOW)
        self.assertIn("proof-artifacts/evidence/", WORKFLOW)
        self.assertIn("pkg-v0.1.0-alpha.7-x86_64-linux-proof", WORKFLOW)
        self.assertIn("retention-days: 7", WORKFLOW)

    def test_production_linux_input_is_manual_fixed_and_not_published(self) -> None:
        self.assertIn("production-linux:", WORKFLOW)
        self.assertIn("inputs.production-linux", WORKFLOW)
        self.assertIn("runs-on: ubuntu-22.04", WORKFLOW)
        self.assertIn("https://releases.happytoolin.com/metadata/1.root.json", WORKFLOW)
        self.assertIn(
            "52523a9bf76dee8e364efc302b733140f850fe377c1cc73a7675b842d28b94e2",
            WORKFLOW,
        )
        self.assertIn("pkg-v0.1.0-alpha.7-production-linux-input", WORKFLOW)
        self.assertIn("pkg-release-index", WORKFLOW)

    def test_linux_uninstall_uses_plain_terminal_exec_status(self) -> None:
        self.assertEqual(
            LINUX_HARNESS.count(
                'docker exec "$container" /usr/local/bin/pkg --yes uninstall'
            ),
            1,
        )
        self.assertIn("uninstall_status=$?", LINUX_HARNESS)
        self.assertIn('exit "$uninstall_status"', LINUX_HARNESS)
        for line in LINUX_HARNESS.splitlines():
            if "uninstall" in line:
                self.assertNotIn("--json", line)
        self.assertNotIn("pkg-after-uninstall", LINUX_HARNESS)
        self.assertNotIn("idempotent uninstall", LINUX_HARNESS)

    def test_macos_proof_stages_both_shared_runtime_archives(self) -> None:
        self.assertIn('runtimes="$RUNNER_TEMP/pkg-proof-runtimes"', MACOS_WORKFLOW)
        self.assertIn('path="$runtimes/$candidate.tar.xz"', MACOS_WORKFLOW)
        self.assertIn(
            '"https://releases.nixos.org/nix/nix-2.34.8/'
            'nix-2.34.8-$candidate.tar.xz"',
            MACOS_WORKFLOW,
        )
        self.assertIn(
            'printf \'%s  %s\\n\' "$expected_sha256" "$path" \\\n'
            "              | shasum -a 256 --check",
            MACOS_WORKFLOW,
        )
        for candidate, digest in (
            (
                "aarch64-darwin",
                "ae3b2b1a74b956110d14dd813bee80ea46626a51ddce28d142e0805379a34acf",
            ),
            (
                "x86_64-linux",
                "2c2e146b80834fe0ca201b51deeb939405b4f18e8d2071bf80b10f8123c50464",
            ),
        ):
            self.assertIn(f"stage_runtime {candidate} \\\n            {digest}", MACOS_WORKFLOW)
        self.assertIn(
            '"$bundle/publication-1" "$runtimes" "$binaries"', MACOS_WORKFLOW
        )
        self.assertIn(
            '"$bundle/publication-2" "$runtimes" "$binaries"', MACOS_WORKFLOW
        )

    def test_production_signing_is_keyless_protected_and_closed(self) -> None:
        self.assertIn("environment: release", PUBLISH_WORKFLOW)
        self.assertIn("contents: write", PUBLISH_WORKFLOW)
        self.assertIn("id-token: write", PUBLISH_WORKFLOW)
        self.assertNotIn("secrets.", PUBLISH_WORKFLOW)
        self.assertIn(
            "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6",
            PUBLISH_WORKFLOW,
        )
        self.assertIn("test \"$draft\" = true", PUBLISH_WORKFLOW)
        self.assertIn("releases/${RELEASE_ID}", PUBLISH_WORKFLOW)
        self.assertNotIn("releases/tags/${RELEASE_TAG}", PUBLISH_WORKFLOW)
        self.assertIn("generated_count != 0 && generated_count", PUBLISH_WORKFLOW)
        self.assertIn('if [[ ! -f "$bundle" ]]', PUBLISH_WORKFLOW)
        self.assertEqual(PUBLISH_WORKFLOW.count("cosign sign-blob"), 1)
        self.assertEqual(PUBLISH_WORKFLOW.count("cosign verify-blob"), 1)
        self.assertIn("--yes", PUBLISH_WORKFLOW)
        self.assertIn("--certificate-identity", PUBLISH_WORKFLOW)
        self.assertIn("--certificate-oidc-issuer", PUBLISH_WORKFLOW)
        self.assertIn("sha256sum --check --strict", PUBLISH_WORKFLOW)
        self.assertIn(".trustedRootSha256", PUBLISH_WORKFLOW)
        self.assertIn('test "${#cli_artifacts[@]}" = 3', PUBLISH_WORKFLOW)
        self.assertIn(".sigstoreBundleSha256", PUBLISH_WORKFLOW)
        self.assertIn("diff -u expected-assets actual-assets", PUBLISH_WORKFLOW)
        self.assertIn("final-assets", PUBLISH_WORKFLOW)
        self.assertIn("gh release upload", PUBLISH_WORKFLOW)
        self.assertIn("gh release edit", PUBLISH_WORKFLOW)
        self.assertNotIn("gh release", WORKFLOW)


if __name__ == "__main__":
    unittest.main()
