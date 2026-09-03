"""Structural safety checks for the DN-1 repeat-run loopback proof."""

import os
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
REPEAT = (ROOT / ".github/workflows/proof-repeat.yml").read_text()
DN16 = (ROOT / ".github/workflows/macos-alpha-proof.yml").read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()
README = (ROOT / "tests/macos-clean-host/REPEAT.md").read_text()
TOOL = (ROOT / "tools/release/serve_pair_loopback.py").read_text()
TOOL_TESTS = (ROOT / "tools/release/test_serve_pair_loopback.py").read_text()
PINNED_USE = re.compile(
    r"^\s*uses:\s*[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}\s*$",
    re.MULTILINE,
)
PENDING_DIGESTS = (
    "PKG_PROOF_PAIR_TARBALL_SHA256",
    "PKG_PROOF_PAIR_SHA256",
    "PKG_PROOF_N_INVENTORY_SHA256",
    "PKG_PROOF_N_PLUS_1_INVENTORY_SHA256",
    "PKG_PROOF_N_ROWS_SHA256",
    "PKG_PROOF_N_PLUS_1_ROWS_SHA256",
)


class RepeatWorkflowTests(unittest.TestCase):
    def job(self, name: str, following: str) -> str:
        return REPEAT.split(f"\n  {name}:\n", 1)[1].split(f"\n  {following}:\n", 1)[0]

    def test_dispatch_is_manual_gated_and_disconnected_from_nightly(self) -> None:
        trigger = REPEAT.split("permissions:", 1)[0]
        self.assertIn("workflow_dispatch:", trigger)
        self.assertNotIn("workflow_call:", trigger)
        self.assertNotIn("schedule:", trigger)
        self.assertNotIn("push:", trigger)
        self.assertNotIn("pull_request:", trigger)
        self.assertIn("DESTROY-PKG-DISPOSABLE-MACOS", trigger)
        for name in ("pair_tag", "from_release", "to_release", "expected_sha"):
            self.assertIn(f"{name}:", trigger)
        self.assertNotIn("proof_channel_url", trigger)
        self.assertNotIn("repeat-proof", DN16)
        self.assertNotIn("macos-alpha-proof", REPEAT)

    def test_the_dispatch_pins_the_new_tag_pair_and_releases(self) -> None:
        validate = self.job("validate-dispatch", "harness")
        for required in (
            "environment: release",
            'test "$PAIR_TAG" = "$PKG_PROOF_PAIR_TAG"',
            'test "$GITHUB_REF" = "refs/tags/$PKG_PROOF_WORKFLOW_TAG"',
            'test "$GITHUB_SHA" = "$EXPECTED_SHA"',
            'test "$GITHUB_WORKFLOW_SHA" = "$EXPECTED_SHA"',
            'test "$target_sha" = "$EXPECTED_SHA"',
            'test "$verified" = true',
            "[[ \"$digest\" =~ ^[0-9a-f]{64}$ ]]",
            "[[ \"$count\" =~ ^[1-9][0-9]*$ ]]",
            "--jq '.draft')\" = false",
        ):
            self.assertIn(required, validate)
        self.assertLess(
            validate.index("[[ \"$count\" =~ ^[1-9][0-9]*$ ]]"),
            validate.index("gh api \"repos/$GITHUB_REPOSITORY/git/ref/tags/"),
        )
        self.assertIn("PKG_PROOF_WORKFLOW_TAG: dn1-proof-workflow-1", REPEAT)
        self.assertIn("PKG_PROOF_PAIR_TAG: dn1-proof-pair-3", REPEAT)
        self.assertIn("PKG_PROOF_PAIR_TARBALL: dn1-proof-pair.tar.gz", REPEAT)
        self.assertIn(
            "PKG_REVIEWED_COMMIT: 56f6782efcd34451c63cd2a940cd8b4e36fd7d44",
            REPEAT,
        )
        self.assertIn('test "$FROM_RELEASE" = v0.1.0-alpha.26', REPEAT)
        self.assertIn('test "$TO_RELEASE" = v0.1.0-alpha.27', REPEAT)
        self.assertNotIn('test "$FROM_RELEASE" = v0.1.0-alpha.24', REPEAT)
        self.assertNotIn('test "$TO_RELEASE" = v0.1.0-alpha.25', REPEAT)

    def test_pair_pins_bind_the_sealed_dn1_proof_pair_3(self) -> None:
        self.assertNotIn("PENDING-DN1-MINT", REPEAT)
        minted = {
            "PKG_PROOF_PAIR_TARBALL_SHA256":
                "aac6aa23d5a76057ce506bc56a0a3e545d4da74f894d59487c02c808f27c16df",
            "PKG_PROOF_PAIR_SHA256":
                "3ee51dd5cad659f7dce01df3bcccbcf5942bcf3ba704d1b07e06602ae8034a77",
            "PKG_PROOF_N_INVENTORY_SHA256":
                "9f52cc950409553494c6acd13c3d4d1cfdf16c19e1b3f557eb0102301356ede1",
            "PKG_PROOF_N_PLUS_1_INVENTORY_SHA256":
                "959d74a0b8e3ec7fd7a27d259a4be43fe25eb0f3ec78ceb0123ba0f75d8dfb99",
            "PKG_PROOF_N_ROWS_SHA256":
                "ffb4ab2658220398b174782ff185cfb618f4a205b5c9db6ea14e42269674e490",
            "PKG_PROOF_N_PLUS_1_ROWS_SHA256":
                "d76f562ae24ada6894d348450449996281afe0d1ea66155c8c6bc9ec1a9daa7f",
        }
        for name, digest in minted.items():
            self.assertIn(f"{name}: {digest}", REPEAT)
        for count, value in (
            ("PKG_PROOF_PAIR_TARBALL_LENGTH", "419431832"),
            ("PKG_PROOF_PAIR_LENGTH", "1101"),
            ("PKG_PROOF_N_INVENTORY_LENGTH", "6085"),
            ("PKG_PROOF_N_PLUS_1_INVENTORY_LENGTH", "6085"),
            ("PKG_PROOF_N_TOTAL_BYTES", "321645504"),
            ("PKG_PROOF_N_PLUS_1_TOTAL_BYTES", "321645391"),
        ):
            self.assertIn(f'{count}: "{value}"', REPEAT)
        self.assertIn(
            "PKG_PROOF_TRUSTED_ROOT_SHA256: "
            "c317d2ad134e0e9efe7c0e836b9b62fa386309e78fa859a516d3ecc943168dd8",
            REPEAT,
        )
        self.assertNotIn(
            "baae3f8b3027f61903b65ce1c47bd9ec756efce4b8b015d56a7c37fd4212fc63",
            REPEAT,
        )

    def test_the_channel_is_loopback_tls_only(self) -> None:
        self.assertIn("PKG_PROOF_CHANNEL_URL: https://127.0.0.1:8443", REPEAT)
        self.assertIn('PKG_PROOF_LOOPBACK_PORT: "8443"', REPEAT)
        self.assertNotIn("trycloudflare", REPEAT)
        phases = self.job("prepare-slot-1", "resume-slot-1")
        self.assertNotIn("PROOF_CHANNEL_URL: ${{ inputs.", REPEAT)
        publish = phases.split(
            "      - name: Publish the loopback channel inside the VM\n", 1
        )[1].split("\n      - name: Trust the disposable loopback CA", 1)[0]
        for required in (
            "serve_pair_loopback.py\" bootstrap",
            "$root/channel-staging\" \"$root/channel\"",
            "$PKG_PROOF_LOOPBACK_PORT",
            "the loopback endpoint answered plaintext HTTP",
            "--cacert \"$state/ca.pem\"",
        ):
            self.assertIn(required, publish)
        trust = phases.split(
            "      - name: Trust the disposable loopback CA\n", 1
        )[1].split("\n      - name: Run the selected proof phase", 1)[0]
        for required in (
            "add-trusted-cert -d -r trustRoot",
            "-k /Library/Keychains/System.keychain \"$ca\"",
            "/usr/bin/security verify-cert -c \"$server\" >/dev/null",
            "find-certificate \\\n            -c pkg-dn1-loopback-ca",
        ):
            self.assertIn(required, trust)

    def test_teardown_is_strict_and_always_runs(self) -> None:
        phases = self.job("prepare-slot-1", "resume-slot-1")
        retire = phases.split(
            "      - name: Retire the loopback channel and CA trust\n", 1
        )[1].split("\n      - name: Upload bounded phase evidence", 1)[0]
        self.assertIn("if: always()", retire)
        for required in (
            "remove-trusted-cert -d \"$ca\"",
            "serve_pair_loopback.py\" stop \"$state\"",
            "test ! -e \"$root/channel\"",
            "the loopback CA is still trusted after teardown",
            "the loopback CA certificate is still in the System keychain",
            "the loopback port is still accepting connections",
            "/usr/bin/nc -z 127.0.0.1 \"$PKG_PROOF_LOOPBACK_PORT\"",
        ):
            self.assertIn(required, retire)
        retire_lines = [line.strip() for line in retire.splitlines()]
        self.assertLess(
            retire_lines.index("if: always()"),
            retire_lines.index("set -euo pipefail"),
        )
        upload = phases.split(
            "      - name: Upload bounded phase evidence\n", 1
        )[1].split("\n  resume-slot-1:", 1)[0]
        self.assertIn("if: always()", upload)

    def test_harness_ships_the_four_file_inventory_and_the_tool_tests(self) -> None:
        payload = "./README.md ./pkg-installer-tests ./prove.sh ./serve_pair_loopback.py"
        harness = self.job("harness", "acquire-inputs")
        for required in (
            "python3 -m unittest proof-source/tests/macos-clean-host/test_workflow.py",
            "python3 -m unittest proof-source/tests/macos-clean-host/test_repeat_workflow.py",
            "python3 -m unittest proof-source/tools/release/test_serve_pair_loopback.py",
            "bash -n proof-source/tests/macos-clean-host/prove.sh",
            "cargo test --locked --release -p pkg-installer --lib --no-run",
            "install -m 0755 ../proof-source/tools/release/serve_pair_loopback.py",
            "install -m 0644 ../proof-source/tests/macos-clean-host/REPEAT.md",
            f"printf '%s\\n' {payload} | LC_ALL=C sort > INVENTORY",
            f"shasum -a 256 {payload} > SHA256SUMS",
        ):
            self.assertIn(required, harness)
        phases = self.job("prepare-slot-1", "resume-slot-1")
        self.assertIn(f"printf '%s\\n' {payload} | LC_ALL=C sort > EXPECTED", phases)
        self.assertIn("chmod 0755 ./prove.sh ./pkg-installer-tests ./serve_pair_loopback.py", phases)

    def test_acquisition_downloads_the_tag_bundle_and_authenticates_everything(self) -> None:
        hosted = self.job("acquire-inputs", "prepare-slot-1")
        for required in (
            "GH_TOKEN: ${{ github.token }}",
            "--jq '.draft')\" = false",
            "-H \"Accept: application/octet-stream\"",
            "tarfile.open(source, \"r:gz\")",
            "non-regular tar member",
            "escaping tar member",
            "tar members exceed the bounded extraction",
            "pair bundle has unexpected top-level entries",
            '"proof pair digest mismatch"',
            '"proof channels use an unexpected trusted root"',
            '"proof tree does not match its inventory"',
            '"proof channel rows digest mismatch"',
            "cosign verify-blob --bundle \"$dir/SHA256SUMS.sigstore.json\"",
            "--certificate-identity \"$identity\" --certificate-oidc-issuer \"$issuer\"",
            "publish-release.yml@refs/tags/dn16-proof-workflow-1",
            "PKG-DN1-HOSTED-ACQUISITION-V1",
        ):
            self.assertIn(required, hosted)
        phases = self.job("prepare-slot-1", "resume-slot-1")
        acquire = phases.split(
            "      - name: Acquire the sealed pair from the proof-pair tag\n", 1
        )[1].split("\n      - name: Publish the loopback channel inside the VM", 1)[0]
        for required in (
            "GH_TOKEN: ${{ github.token }}",
            "-H \"Accept: application/octet-stream\"",
            "test ! -e \"$staging\"",
            '"proof tree does not match its inventory"',
            "PKG-DN1-VM-ACQUISITION-V1",
            "tarball_bytes=$PKG_PROOF_PAIR_TARBALL_LENGTH",
            "channel_url=$PKG_PROOF_CHANNEL_URL",
            "'status=complete'",
        ):
            self.assertIn(required, acquire)
        self.assertNotIn("trycloudflare", acquire)

    def test_prove_sh_binds_the_repeat_pair_and_the_full_tree(self) -> None:
        for required in (
            "PKG_PROOF_PAIR_TAG PKG_PROOF_PAIR_TARBALL_LENGTH",
            '[ "$PKG_PROOF_FROM_RELEASE" = v0.1.0-alpha.26 ]',
            '[ "$PKG_PROOF_TO_RELEASE" = v0.1.0-alpha.27 ]',
            "'schema=PKG-DN1-VM-ACQUISITION-V1'",
            '"pair_tag=$PKG_PROOF_PAIR_TAG"',
            '"tarball_bytes=$PKG_PROOF_PAIR_TARBALL_LENGTH"',
            '"channel_url=$PKG_PROOF_CHANNEL_URL"',
            "require(actual == set(files), \"proof tree does not match its inventory\")",
            "\"proof tree has missing or extra directories\"",
        ):
            self.assertIn(required, PROOF)
        for forbidden in (
            "PKG_PROOF_INPUT_BYTES",
            "PKG_PROOF_RESPONSE_BYTES",
            "logical_fetches=21",
            "PKG-DN16-VM-ACQUISITION-V1",
            "compact proof",
            "actual == proof_inputs",
        ):
            self.assertNotIn(forbidden, PROOF)
        self.assertIn('"channel_url=$PKG_PROOF_CHANNEL_URL"', PROOF)
        self.assertNotIn("alpha.24", PROOF)
        self.assertNotIn("alpha.25", PROOF)

    def test_the_harness_is_run_from_the_workflow(self) -> None:
        phases = self.job("prepare-slot-1", "resume-slot-1")
        run_phase = phases.split(
            "      - name: Run the selected proof phase\n", 1
        )[1].split("\n      - name: Retire the loopback channel", 1)[0]
        for required in (
            "PKG_PROOF_FROM_RELEASE: ${{ inputs.from_release }}",
            "PKG_PROOF_TO_RELEASE: ${{ inputs.to_release }}",
            "PKG_PROOF_PAIR_TAG: ${{ inputs.pair_tag }}",
            "PKG_PROOF_ROOT: ${{ runner.temp }}/pkg-macos-proof",
            "PKG_PROOF_REBOOT_MARKER: /var/tmp/pkg-disposable-macos-reboot-v2",
            "harness/prove.sh",
        ):
            self.assertIn(required, run_phase)
        self.assertNotIn("PKG_PROOF_CHANNEL_URL: ${{ inputs.", REPEAT)

    def test_jobs_have_the_exact_two_slot_two_phase_order(self) -> None:
        expected = (
            ("prepare-slot-1", "resume-slot-1", "needs: [validate-dispatch, harness, acquire-inputs]"),
            ("resume-slot-1", "prepare-slot-2", "needs: prepare-slot-1"),
            ("prepare-slot-2", "resume-slot-2", "needs: [resume-slot-1, harness, acquire-inputs]"),
            ("resume-slot-2", "aggregate", "needs: prepare-slot-2"),
        )
        for job, following, need in expected:
            self.assertIn(need, self.job(job, following))
        self.assertNotIn("strategy:", REPEAT)
        self.assertNotIn("matrix:", REPEAT)
        positions = [REPEAT.index(f"\n  {job}:\n") for job, _, _ in expected]
        self.assertEqual(positions, sorted(positions))
        for job, following in (
            ("prepare-slot-1", "resume-slot-1"),
            ("resume-slot-1", "prepare-slot-2"),
            ("prepare-slot-2", "resume-slot-2"),
            ("resume-slot-2", "aggregate"),
        ):
            self.assertIn("timeout-minutes: 360", self.job(job, following))
        self.assertEqual(REPEAT.count("timeout-minutes: 360"), 4)

    def test_each_phase_keeps_the_exact_disposable_runner_identity(self) -> None:
        for slot in (1, 2):
            for phase_name in ("prepare", "resume"):
                following = {
                    (1, "prepare"): "resume-slot-1",
                    (1, "resume"): "prepare-slot-2",
                    (2, "prepare"): "resume-slot-2",
                    (2, "resume"): "aggregate",
                }[(slot, phase_name)]
                section = self.job(f"{phase_name}-slot-{slot}", following)
                self.assertIn(f"pkg-disposable-macos-proof-{slot}", section)
                self.assertIn(
                    f"PKG_PROOF_EXPECTED_RUNNER: pkg-dn16-proof-runner-{slot}", section
                )
                self.assertIn(f'PKG_PROOF_LIFECYCLE_RUN: "{slot}"', section)
                self.assertIn(f"PKG_PROOF_PHASE: {phase_name}", section)
                self.assertIn(
                    "steps: &proof-phase-steps" if (slot, phase_name) == (1, "prepare")
                    else "steps: *proof-phase-steps",
                    section,
                )
        self.assertIn('test "$RUNNER_NAME" = "$PKG_PROOF_EXPECTED_RUNNER"', REPEAT)
        self.assertIn("kern.hv_vmm_present", REPEAT)
        self.assertIn("VirtualMac*", REPEAT)

    def test_aggregate_verdict_uses_digest_level_evidence_only(self) -> None:
        aggregate = REPEAT.split("\n  aggregate:\n", 1)[1]
        for required in (
            "needs: [prepare-slot-1, resume-slot-1, prepare-slot-2, resume-slot-2]",
            'test "$PREPARE_1_RESULT" = success',
            'test "$RESUME_1_RESULT" = success',
            'test "$PREPARE_2_RESULT" = success',
            'test "$RESUME_2_RESULT" = success',
            "the exact four phase evidence artifacts are required",
            "runner identity changed within a slot",
            "VM identity changed within a slot",
            "the boot UUID did not change within the slot",
            "the two slots used the same VM nonce",
            "PKG-DN1-HOSTED-ACQUISITION-V1",
            "PKG-DN1-VM-ACQUISITION-V1",
            "PKG-DN1-LOOPBACK-V1",
            "PKG-DN1-LOOPBACK-TRUST-V1",
            "hosted acquisition digest mismatch",
            "the loopback channel evidence does not match the sealed pair",
        ):
            self.assertIn(required, aggregate)
        self.assertNotIn("sudo", aggregate)
        self.assertNotIn("prove.sh", aggregate)
        self.assertNotIn('"run_id"', aggregate)
        self.assertNotIn("GITHUB_RUN_ID", aggregate)
        self.assertNotIn("GITHUB_RUN_ATTEMPT", aggregate)
        self.assertIn("int(env[\"PKG_PROOF_PAIR_LENGTH\"])", aggregate)
        self.assertIn("int(env[\"PKG_PROOF_N_INVENTORY_LENGTH\"])", aggregate)

    def test_external_actions_are_immutable_commit_pins(self) -> None:
        external_uses = [
            line
            for line in REPEAT.splitlines()
            if "uses:" in line and "./.github/workflows/" not in line
        ]
        self.assertTrue(external_uses)
        self.assertTrue(all(PINNED_USE.fullmatch(line) for line in external_uses))
        for pinned in (
            "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd",
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
            "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6",
        ):
            self.assertIn(pinned, REPEAT)

    def test_the_loopback_tool_is_self_contained_and_fail_closed(self) -> None:
        for required in (
            "validate_pair",
            "generate_certificate_material",
            "basicConstraints = critical,CA:TRUE",
            "subjectAltName = IP:{ip},DNS:{dns}",
            "ssl.PROTOCOL_TLS_SERVER",
            "minimum_version = ssl.TLSVersion.TLSv1_2",
            "wrap_socket(connection, server_side=True)",
            "LOOPBACK_IP = \"127.0.0.1\"",
            "refusing to remove a symlinked tree",
            "refusing a foreign process",
            "publication must be a sibling of the staging directory",
            "loopback endpoint serves different bytes",
        ):
            self.assertIn(required, TOOL)
        for forbidden in ("trycloudflare", "cloudflared", "http://"):
            self.assertNotIn(forbidden, TOOL)
        self.assertNotIn("verify_remote", TOOL)

    def test_documentation_prepares_the_operator_and_names_the_blockers(self) -> None:
        for required in (
            "Do not destroy or replace the VM.",
            "Reboot the same VM.",
            "Do not change its instance nonce.",
            "Register the same runner name and label",
            "Only then create slot 2.",
            "Do not dispatch the workflow again.",
            "dn1-proof-workflow-1",
            "dn1-proof-pair-1",
            "mint_dn1_proof_pair.sh",
            "PENDING-DN1-MINT",
            "127.0.0.1:8443",
            "remove-trusted-cert",
        ):
            self.assertIn(required, README)


if __name__ == "__main__":
    unittest.main()
