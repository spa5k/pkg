"""Structural safety checks for the destructive macOS lifecycle proof."""

import os
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = Path(
    os.environ.get(
        "PKG_MACOS_PROOF_WORKFLOW",
        ROOT / ".github/workflows/macos-alpha-proof.yml",
    )
)
WORKFLOW = WORKFLOW_PATH.read_text()
NIGHTLY = (WORKFLOW_PATH.parent / "nightly.yml").read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()
README = (ROOT / "tests/macos-clean-host/README.md").read_text()


class MacOsProofWorkflowTests(unittest.TestCase):
    def test_is_manual_and_default_disabled(self) -> None:
        trigger = WORKFLOW.split("permissions:", 1)[0]
        self.assertIn("workflow_dispatch:", trigger)
        self.assertNotIn("workflow_call:", trigger)
        self.assertNotIn("schedule:", trigger)
        self.assertIn("production_environment:", trigger)
        self.assertIn("default: true", trigger)
        self.assertIn("DESTROY-PKG-DISPOSABLE-MACOS", trigger)
        self.assertNotIn("macos-alpha-proof", NIGHTLY)

    def test_uses_two_distinct_disposable_apple_silicon_runners(self) -> None:
        self.assertEqual(WORKFLOW.count("\n  prove:\n"), 1)
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1].split("\n  aggregate:\n", 1)[0]
        self.assertIn('runs-on: [self-hosted, macOS, ARM64, "${{ matrix.runner_label }}"]', proof_job)
        self.assertEqual(proof_job.count("lifecycle_run:"), 2)
        self.assertEqual(proof_job.count("pkg-disposable-macos-proof-1"), 1)
        self.assertEqual(proof_job.count("pkg-disposable-macos-proof-2"), 1)
        self.assertEqual(proof_job.count("pkg-dn16-proof-runner-1"), 1)
        self.assertEqual(proof_job.count("pkg-dn16-proof-runner-2"), 1)
        self.assertIn('test "$RUNNER_NAME" = "${{ matrix.runner_name }}"', proof_job)
        self.assertIn("PKG-DN16-DISPOSABLE-V1:${GITHUB_RUN_ID}:${{ matrix.lifecycle_run }}", proof_job)
        self.assertIn("/var/tmp/pkg-disposable-macos-instance", proof_job)
        self.assertIn("^PKG-DN16-INSTANCE-V1:([0-9a-f]{64})$", proof_job)
        self.assertIn("/var/tmp/pkg-disposable-macos-reboot-v2", proof_job)
        self.assertIn(
            "PKG-DN16-REBOOT-V2:${GITHUB_RUN_ID}:${{ matrix.lifecycle_run }}:${RUNNER_NAME}:${instance_nonce}:",
            proof_job,
        )
        self.assertIn("test \"$(wc -l < \"$reboot_marker\")\" -eq 1", proof_job)
        self.assertIn("[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}", proof_job)
        self.assertIn('test "$prior_boot" != "$current_boot"', proof_job)
        self.assertIn('test "$reboot_age" -le 300', proof_job)
        self.assertIn("kern.hv_vmm_present", proof_job)
        self.assertIn("VirtualMac*", proof_job)
        self.assertIn("root:wheel:600", proof_job)
        self.assertIn("different virtual machines", README)

    def test_destructive_slots_run_sequentially_in_one_workflow_run(self) -> None:
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1].split("\n  aggregate:\n", 1)[0]
        self.assertIn("fail-fast: false", proof_job)
        self.assertIn("max-parallel: 1", proof_job)
        self.assertLess(
            proof_job.index("lifecycle_run: 1"), proof_job.index("lifecycle_run: 2")
        )
        self.assertIn("same GitHub workflow run", README)
        self.assertIn("Do not register slot 2", README)
        self.assertIn("Do not enable or dispatch the workflow again", README)
        self.assertEqual(README.count("matrix job has a terminal result"), 2)
        self.assertEqual(README.count("artifact uploads for that result completed"), 2)
        self.assertEqual(README.count("ephemeral runner registration is absent"), 2)

    def test_preflight_is_retained_before_the_destructive_gate(self) -> None:
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1].split("\n  aggregate:\n", 1)[0]
        preflight = proof_job.index("Initialize bounded preflight evidence")
        gate = proof_job.index("Refuse an unsafe host before download or mutation")
        self.assertLess(preflight, gate)
        self.assertIn("preflight.txt", proof_job)

    def test_destructive_host_receives_no_source_or_build_tools(self) -> None:
        proof_job = WORKFLOW.split("\n  prove:\n", 1)[1].split("\n  aggregate:\n", 1)[0]
        for forbidden in ("actions/checkout", "cargo ", "secrets.", "gh release create", "gh release upload"):
            self.assertNotIn(forbidden, proof_job)
        self.assertNotIn("publish: true", proof_job)
        gate = proof_job.index("Refuse an unsafe host before download or mutation")
        download = proof_job.index("Download proof-only harness")
        candidates = proof_job.index("Download and authenticate signed release inputs")
        self.assertLess(gate, download)
        self.assertLess(gate, candidates)
        self.assertIn('test "$from_source_commit" = "$PKG_REVIEWED_COMMIT"', proof_job)
        self.assertIn('test "$to_source_commit" = "$PKG_REVIEWED_COMMIT"', proof_job)
        self.assertIn("from-source-commit.txt", proof_job)
        self.assertIn("to-source-commit.txt", proof_job)

    def test_harness_inventory_and_short_evidence_retention_are_exact(self) -> None:
        self.assertIn("name: pkg-macos-proof-harness", WORKFLOW)
        self.assertIn("ref: ${{ github.sha }}", WORKFLOW)
        self.assertIn('test "$(git -C proof-source rev-parse HEAD)" = "$GITHUB_SHA"', WORKFLOW)
        self.assertIn("./README.md ./pkg-installer-tests ./prove.sh", WORKFLOW)
        self.assertIn("shasum -a 256 pkg-installer-tests prove.sh README.md", WORKFLOW)
        self.assertGreaterEqual(WORKFLOW.count("retention-days: 3"), 3)
        evidence = WORKFLOW.split("name: Upload bounded proof evidence", 1)[1]
        self.assertIn("if: always()", evidence.split("- name:", 1)[0])
        self.assertNotIn("retention-days: 4", WORKFLOW)

    def test_signed_checksums_bind_each_release_input_before_use(self) -> None:
        inputs = WORKFLOW.split(
            "- name: Download and authenticate signed release inputs", 1
        )[1].split("- name: Run the destructive proof", 1)[0]
        self.assertIn("SHA256SUMS.sigstore.json", inputs)
        verification = 'cosign verify-blob --bundle "$dir/SHA256SUMS.sigstore.json"'
        self.assertIn(verification, inputs)
        self.assertLess(
            inputs.index(verification),
            inputs.index('for asset in "pkg-$version-preview.pkg"'),
        )
        self.assertIn(
            'for asset in "pkg-$version-preview.pkg" pkg-aarch64-darwin release-manifest.json',
            inputs,
        )
        self.assertIn('manifest.get("releaseId") != sys.argv[2]', inputs)
        identity = (
            "https://github.com/spa5k/pkg/.github/workflows/"
            "publish-release.yml@refs/tags/dn16-proof-workflow-1"
        )
        self.assertIn(f'identity="{identity}"', inputs)
        self.assertNotIn("@refs/heads/main", inputs)

    def test_hosted_aggregation_requires_distinct_runner_evidence(self) -> None:
        aggregate = WORKFLOW.split("\n  aggregate:\n", 1)[1]
        self.assertIn("runs-on: ubuntu-24.04", aggregate)
        self.assertIn("needs: prove", aggregate)
        self.assertIn(
            "if: ${{ always() && needs.prove.result != 'skipped' }}", aggregate
        )
        self.assertIn("pattern: pkg-macos-lifecycle-evidence-*", aggregate)
        self.assertIn('test "$PROVE_RESULT" = success', aggregate)
        self.assertIn("runner names are not distinct", aggregate)
        self.assertIn("reimage nonces are not distinct", aggregate)
        self.assertNotIn("sudo", aggregate)
        self.assertNotIn("prove.sh", aggregate)

    def test_harness_has_current_product_boundaries_only(self) -> None:
        for required in (
            "org.pkg.root-helper",
            "org.pkg.nix-broker",
            "/opt/pkg/nix",
            "--repair-product-assets",
            "live uninstall requires plain output",
            "determinate-handoff-v1.json",
            "PKG-DN16-REBOOT-V2",
        ):
            self.assertIn(required, PROOF)
        for obsolete in (
            "diskutil",
            "org.pkg.store-volume",
            "org.pkg.nix-daemon",
            "pkg-proof-server",
            "security add-trusted-cert",
            "APFSContainerSize",
            "/opt/pkg/nix/current",
        ):
            self.assertNotIn(obsolete, PROOF)
        self.assertEqual(PROOF.count("org.pkg.root-helper.plist"), PROOF.count("org.pkg.nix-broker.plist"))

    def test_proof_has_bounded_evidence_and_no_second_download_scheme(self) -> None:
        self.assertIn("/usr/bin/tail -c 65536", PROOF)
        self.assertIn("expected-results.tsv", PROOF)
        self.assertIn("runner\tfresh-runner-reboot\tpass", PROOF)
        self.assertIn("compiled\tprocess-handoff-and-ordering\tpass", PROOF)
        self.assertIn("external\tstaged-channel-upgrade\tblocked", PROOF)
        self.assertIn("external\tlifecycle-reboot-recovery\tblocked", PROOF)
        self.assertNotIn("native\toffline-product-upgrade\tpass", PROOF)
        self.assertIn("native\tterminal-uninstall-completion\tpass", PROOF)
        self.assertNotIn("real-reboot\tpass", PROOF)
        self.assertIn("candidate/from", PROOF)
        self.assertIn("candidate/to", PROOF)
        self.assertIn("$side-selected-sha256.txt", PROOF)
        self.assertNotIn("curl ", PROOF)
        self.assertNotIn("gh ", PROOF)
        self.assertNotIn("openssl", PROOF)
        self.assertNotIn("Keychain", PROOF)
        self.assertIn("8ffd325a4be12a998f3a5684097b57841a11540e", PROOF)
        self.assertIn("90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b", PROOF)
        self.assertIn("handoff_before", PROOF)
        self.assertIn("It does not prove product lifecycle recovery across a reboot.", README)
        self.assertIn("The manual GitHub workflow is the only supported entry point.", README)
        self.assertNotIn("run `prove.sh`", README)
        self.assertIn("authenticates each `SHA256SUMS`", README)
        self.assertIn("authenticated `SHA256SUMS` binds", README)
        self.assertIn("SHA256SUMS.sigstore.json", WORKFLOW)
        self.assertRegex(
            WORKFLOW,
            r'cosign verify-blob[\s\S]+SHA256SUMS\.sigstore\.json[\s\S]+"\$dir/SHA256SUMS"',
        )

    def test_reboot_marker_binds_one_fresh_runner_job(self) -> None:
        for required in (
            "preflight.txt",
            "/var/tmp/pkg-disposable-macos-instance",
            "GITHUB_RUN_ID",
            "RUNNER_NAME",
            "preflight_nonce",
            "reboot_slot",
            "reboot_runner",
            "reboot_nonce",
            "reboot_time",
            "root:wheel:600",
            '"$reboot_age" -le 300',
            '"$marker_age" -le 300',
            "^[0-9a-f]{64}$",
        ):
            self.assertIn(required, PROOF)
        self.assertIn("/usr/bin/wc -l", PROOF)
        self.assertIn("/usr/bin/tail -c 1", PROOF)
        for binding in (
            '"$preflight_nonce" = "$instance_nonce"',
            '"$reboot_run" = "$GITHUB_RUN_ID"',
            '"$reboot_slot" = "$PKG_PROOF_LIFECYCLE_RUN"',
            '"$reboot_runner" = "$RUNNER_NAME"',
            '"$reboot_nonce" = "$instance_nonce"',
            '[ -z "$reboot_extra" ]',
        ):
            self.assertIn(binding, PROOF)
        self.assertIn("trusted scheduler must receive the GitHub run ID", README)

    def test_release_inputs_do_not_claim_a_live_channel_upgrade(self) -> None:
        self.assertIn('"$from_pkg_sha" != "$to_pkg_sha"', PROOF)
        self.assertIn('manifest["releaseId"] == release', PROOF)
        self.assertIn("pkgutil --expand-full", PROOF)
        self.assertNotIn("capture offline-upgrade", PROOF)
        self.assertIn("The public packages resolve one live channel.", PROOF)

    def test_third_party_actions_are_commit_pinned(self) -> None:
        for line in WORKFLOW.splitlines():
            if "uses: actions/" in line:
                revision = line.rsplit("@", 1)[-1]
                self.assertEqual(len(revision), 40)
                int(revision, 16)


if __name__ == "__main__":
    unittest.main()
