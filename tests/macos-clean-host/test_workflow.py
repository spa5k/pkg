"""Structural safety checks for the destructive macOS lifecycle proof."""

import os
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = Path(
    os.environ.get("PKG_MACOS_PROOF_WORKFLOW", ROOT / ".github/workflows/macos-alpha-proof.yml")
)
WORKFLOW = WORKFLOW_PATH.read_text()
NIGHTLY = (WORKFLOW_PATH.parent / "nightly.yml").read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()
README = (ROOT / "tests/macos-clean-host/README.md").read_text()


class MacOsProofWorkflowTests(unittest.TestCase):
    def job(self, name: str, following: str) -> str:
        return WORKFLOW.split(f"\n  {name}:\n", 1)[1].split(f"\n  {following}:\n", 1)[0]

    def test_dispatch_is_manual_disabled_and_immutable(self) -> None:
        trigger = WORKFLOW.split("permissions:", 1)[0]
        self.assertIn("workflow_dispatch:", trigger)
        self.assertNotIn("workflow_call:", trigger)
        self.assertNotIn("schedule:", trigger)
        self.assertIn("default: true", trigger)
        self.assertIn("DESTROY-PKG-DISPOSABLE-MACOS", trigger)
        self.assertNotIn("proof_pair_sha256:", trigger)
        self.assertNotIn("macos-alpha-proof", NIGHTLY)
        validate = self.job("validate-dispatch", "harness")
        for required in (
            "environment: release",
            'test "$GITHUB_REF" = "refs/tags/$PKG_PROOF_WORKFLOW_TAG"',
            'test "$GITHUB_SHA" = "$EXPECTED_SHA"',
            'test "$GITHUB_WORKFLOW_SHA" = "$EXPECTED_SHA"',
            'test "$target_sha" = "$EXPECTED_SHA"',
            'test "$verified" = true',
        ):
            self.assertIn(required, validate)
        self.assertIn("PKG_PROOF_WORKFLOW_TAG: dn16-macos-proof-workflow-6", WORKFLOW)
        self.assertIn(
            "PKG_PROOF_PAIR_SHA256: "
            "0880b6d78cf671672e55496978d0f5ab1d9feb9f5ca2f8389608f7168b637785",
            WORKFLOW,
        )

    def test_jobs_have_an_explicit_two_slot_two_phase_order(self) -> None:
        expected = (
            ("prepare-slot-1", "resume-slot-1", "needs: [validate-dispatch, harness, acquire-inputs]"),
            ("resume-slot-1", "prepare-slot-2", "needs: prepare-slot-1"),
            ("prepare-slot-2", "resume-slot-2", "needs: [resume-slot-1, harness, acquire-inputs]"),
            ("resume-slot-2", "aggregate", "needs: prepare-slot-2"),
        )
        for job, following, need in expected:
            section = self.job(job, following)
            self.assertIn(need, section)
        self.assertNotIn("strategy:", WORKFLOW)
        self.assertNotIn("matrix:", WORKFLOW)
        positions = [WORKFLOW.index(f"\n  {job}:\n") for job, _, _ in expected]
        self.assertEqual(positions, sorted(positions))

    def test_each_destructive_phase_allows_six_hours(self) -> None:
        phases = (
            ("prepare-slot-1", "resume-slot-1"),
            ("resume-slot-1", "prepare-slot-2"),
            ("prepare-slot-2", "resume-slot-2"),
            ("resume-slot-2", "aggregate"),
        )
        for job, following in phases:
            self.assertIn("timeout-minutes: 360", self.job(job, following))
        self.assertEqual(WORKFLOW.count("timeout-minutes: 360"), len(phases))

    def test_each_phase_has_the_exact_runner_identity(self) -> None:
        for slot in (1, 2):
            for phase in ("prepare", "resume"):
                following = {
                    (1, "prepare"): "resume-slot-1",
                    (1, "resume"): "prepare-slot-2",
                    (2, "prepare"): "resume-slot-2",
                    (2, "resume"): "aggregate",
                }[(slot, phase)]
                section = self.job(f"{phase}-slot-{slot}", following)
                self.assertIn(f"pkg-disposable-macos-proof-{slot}", section)
                self.assertIn(
                    f"PKG_PROOF_EXPECTED_RUNNER: pkg-dn16-proof-runner-{slot}", section
                )
                self.assertIn(f'PKG_PROOF_LIFECYCLE_RUN: "{slot}"', section)
                self.assertIn(f"PKG_PROOF_PHASE: {phase}", section)
                self.assertIn(
                    "steps: &proof-phase-steps" if (slot, phase) == (1, "prepare")
                    else "steps: *proof-phase-steps",
                    section,
                )
        self.assertIn('test "$RUNNER_NAME" = "$PKG_PROOF_EXPECTED_RUNNER"', WORKFLOW)
        self.assertIn("kern.hv_vmm_present", WORKFLOW)
        self.assertIn("VirtualMac*", WORKFLOW)
        self.assertIn(
            "name: pkg-macos-lifecycle-evidence-${{ env.PKG_PROOF_LIFECYCLE_RUN }}-"
            "${{ env.PKG_PROOF_PHASE }}",
            WORKFLOW,
        )

    def test_acquisition_keeps_the_full_sealed_channel_validation(self) -> None:
        acquire = self.job("acquire-inputs", "prepare-slot-1")
        for required in (
            "--proto '=https'",
            "--max-filesize",
            'test "$response" = "200 $url"',
            "proof-pair.json",
            '[[ "$PROOF_PAIR_SHA256" =~ ^[0-9a-f]{64}$ ]]',
            "channel-files.tsv",
            'fetch "$PROOF_CHANNEL_URL/$name/$path"',
            "proof inventory has missing or extra entries",
            "proof-inputs/pkg-aarch64-darwin.sigstore.json",
            'pair["productCommit"] == reviewed_commit',
            "SHA256SUMS.sigstore.json",
            'cosign verify-blob --bundle "$dir/SHA256SUMS.sigstore.json"',
            "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6",
            "pkg-macos-authenticated-inputs",
        ):
            self.assertIn(required, acquire)
        self.assertNotIn("--location", acquire)
        self.assertNotIn("or .isPrerelease", acquire)
        self.assertNotIn("GH_TOKEN", acquire)
        self.assertNotIn("gh release", acquire)
        self.assertNotIn("gh api", acquire)
        self.assertNotIn("/releases", acquire)
        self.assertNotIn("gh release", WORKFLOW)
        self.assertNotIn("/releases", WORKFLOW)
        self.assertNotIn("assert ", acquire)
        self.assertIn("python3 -I -", acquire)

    def test_harness_inventory_is_explicit_and_matches_the_verifier(self) -> None:
        payload = "./README.md ./pkg-installer-tests ./prove.sh"
        harness = self.job("harness", "acquire-inputs")
        phase = self.job("prepare-slot-1", "resume-slot-1")
        producer = harness.split('(\n            cd "$out"\n', 1)[1].split(
            "\n          )", 1
        )[0]
        self.assertIn(f"printf '%s\\n' {payload} \\", producer)
        self.assertIn("| LC_ALL=C sort > INVENTORY", producer)
        self.assertIn(f"printf '%s\\n' {payload} \\", phase)
        self.assertIn("| LC_ALL=C sort > EXPECTED", phase)
        self.assertIn(
            f"shasum -a 256 {payload} > SHA256SUMS",
            producer,
        )
        self.assertIn("awk '{print $2}' SHA256SUMS | LC_ALL=C sort > CHECKSUM_PATHS", phase)
        self.assertIn("cmp EXPECTED CHECKSUM_PATHS", phase)
        self.assertIn('"$harness/pkg-installer-tests" --exact', PROOF)
        self.assertNotIn("find . -type f", harness)
        self.assertEqual(producer.count("INVENTORY"), 1)
        self.assertEqual(producer.count("SHA256SUMS"), 1)

    def test_harness_modes_are_restored_only_after_hash_verification(self) -> None:
        phase = self.job("prepare-slot-1", "resume-slot-1")
        verifier = phase.split("      - name: Verify the harness inventory\n", 1)[1].split(
            "\n      - name: Download authenticated proof inputs", 1
        )[0]
        checksum = "shasum -a 256 --check SHA256SUMS"
        executable = "chmod 0755 ./prove.sh ./pkg-installer-tests"
        controls = (
            "chmod 0644 ./README.md ./INVENTORY ./SHA256SUMS ./EXPECTED "
            "./CHECKSUM_PATHS"
        )
        self.assertLess(verifier.index(checksum), verifier.index(executable))
        self.assertLess(verifier.index(checksum), verifier.index(controls))
        chmods = [line.strip() for line in phase.splitlines() if line.strip().startswith("chmod")]
        self.assertEqual(chmods, [executable, controls])

    def test_prepare_creates_state_under_n_before_the_offline_upgrade(self) -> None:
        install = PROOF.index('capture package-state-install "$pkg"')
        stop = PROOF.index("persist_prepare_state", install)
        upgrade = PROOF.index('capture staged-channel-upgrade /usr/bin/sudo "$to_installer"')
        continuation = PROOF.index("write_continuation", upgrade)
        self.assertLess(install, stop)
        self.assertLess(stop, upgrade)
        self.assertLess(upgrade, continuation)
        for required in (
            "native\trepresentative-package-state\tpass",
            "assert_services_offline",
            "compare_prepare_state",
            "native\tprepare-state-preserved\tpass",
            "runner\tcontinuation-recorded\tpass",
        ):
            self.assertIn(required, PROOF)

    def test_prepare_retains_the_full_clean_host_boundary(self) -> None:
        for required in (
            "preflight-launchd-labels.txt",
            "preflight-users.txt",
            "preflight-groups.txt",
            "/private/etc/synthetic.conf",
            "/private/etc/fstab",
            ".nix-profile",
            "/opt/homebrew/bin",
            "require_unloaded",
            '"$status" -eq 113',
        ):
            self.assertIn(required, PROOF)

    def test_continuation_is_root_owned_exact_and_outside_runner_temp(self) -> None:
        for required in (
            "continuation=/private/var/db/pkg-dn16-proof-continuation-v1",
            "continuation_state=/private/var/db/pkg-dn16-proof-continuation-state-v1",
            "/usr/bin/install -o root -g wheel -m 0600",
            "root:wheel:600",
            "root:wheel:700",
            "schema=PKG-DN16-CONTINUATION-V1",
            "run_id=$GITHUB_RUN_ID",
            "run_attempt=$GITHUB_RUN_ATTEMPT",
            "runner_name=$RUNNER_NAME",
            "instance_nonce=$instance_nonce",
            "prepare_boot_uuid=$prepare_boot",
            "workflow_sha=${GITHUB_WORKFLOW_SHA:-}",
            "proof_pair_sha256=$PKG_PROOF_PAIR_SHA256",
            "status=awaiting-reboot",
        ):
            self.assertIn(required, PROOF)
        self.assertIn('set(records) != expected', PROOF)
        self.assertIn('[ "$old_boot" != "$current_boot" ]', PROOF)

    def test_all_protected_marker_and_continuation_reads_use_sudo_n(self) -> None:
        phase = self.job("prepare-slot-1", "resume-slot-1")
        for required in (
            '/usr/bin/sudo -n /usr/bin/true',
            '/usr/bin/sudo -n /bin/test -f "$disposable"',
            '/usr/bin/sudo -n /bin/test ! -L "$disposable"',
            '/usr/bin/sudo -n /usr/bin/stat -f \'%Su:%Sg:%Lp\' "$disposable"',
            '/usr/bin/sudo -n /bin/cat "$disposable"',
            '/usr/bin/sudo -n /bin/test -f "$instance_marker"',
            '/usr/bin/sudo -n /usr/bin/stat -f \'%z\' "$instance_marker"',
            '/usr/bin/sudo -n /bin/cat "$instance_marker"',
            '/usr/bin/sudo -n /bin/test -f "$continuation"',
            '/usr/bin/sudo -n /usr/bin/stat -f \'%z\' "$continuation"',
        ):
            self.assertIn(required, phase)
        for required in (
            '/usr/bin/sudo -n /usr/bin/true',
            '/usr/bin/sudo -n /bin/cat "$instance_marker"',
            '/usr/bin/sudo -n /bin/cat "$disposable"',
            '/usr/bin/sudo -n /bin/cat "$PKG_PROOF_REBOOT_MARKER"',
            '/usr/bin/sudo -n /usr/bin/tail -c 1 "$PKG_PROOF_REBOOT_MARKER"',
            '/usr/bin/sudo -n /bin/cat "$continuation"',
            '/usr/bin/sudo -n /bin/mkdir -p "$continuation_state"',
            '/usr/bin/sudo -n /bin/chown root:wheel "$continuation_state"',
            '/usr/bin/sudo -n /bin/chmod 0700 "$continuation_state"',
            '/usr/bin/sudo -n /usr/bin/install -o root -g wheel -m 0600',
            '/usr/bin/sudo -n /bin/test -f "$path"',
            '/usr/bin/sudo -n /bin/test ! -L "$path"',
            '/usr/bin/sudo -n /usr/bin/stat -f \'%Su:%Sg:%Lp\' "$path"',
            '/usr/bin/sudo -n /usr/bin/shasum -a 256 "$path"',
            '/usr/bin/sudo -n /usr/bin/cmp "$continuation_state/$name.before"',
            '/usr/bin/sudo -n /bin/rm',
            '/usr/bin/sudo -n /bin/rmdir "$continuation_state"',
        ):
            self.assertIn(required, PROOF)
        self.assertLess(PROOF.index("/usr/bin/sudo -n /usr/bin/true"),
                        PROOF.index("instance_marker="))
        for forbidden in (
            '$(cat "$disposable")',
            '$(cat "$instance_marker")',
            '$(/bin/cat "$disposable")',
            '$(/bin/cat "$instance_marker")',
            '$(/bin/cat "$PKG_PROOF_REBOOT_MARKER")',
            '<"$PKG_PROOF_REBOOT_MARKER"',
        ):
            self.assertNotIn(forbidden, WORKFLOW + PROOF)
        continuation_proof = PROOF.split("verify_continuation() {", 1)[1].split(
            "snapshot_uninstall_boundary()", 1
        )[0]
        for forbidden in (
            '[ -f "$path" ]',
            '[ ! -L "$path" ]',
            '$(/usr/bin/stat -f \'%Su:%Sg:%Lp\' "$path")',
        ):
            self.assertNotIn(forbidden, continuation_proof)
        for forbidden in (
            '/usr/bin/sudo /bin/mkdir -p "$continuation_state"',
            '/usr/bin/sudo /bin/chown root:wheel "$continuation_state"',
            '/usr/bin/sudo /bin/chmod 0700 "$continuation_state"',
            '/usr/bin/sudo /usr/bin/install -o root -g wheel -m 0600',
            '/usr/bin/sudo /bin/test -f "$continuation"',
            '/usr/bin/sudo /bin/test ! -L "$continuation"',
            '/usr/bin/sudo /bin/cat "$continuation"',
            '/usr/bin/sudo /usr/bin/cmp "$continuation_state/$name.before"',
            '/usr/bin/sudo /usr/bin/shasum -a 256 \\\n'
            '                "$continuation_state/$name.before"',
            '/usr/bin/sudo /bin/rm \\\n',
            '/usr/bin/sudo /bin/rmdir "$continuation_state"',
        ):
            self.assertNotIn(forbidden, PROOF)
        for forbidden in (
            '/usr/bin/sudo /bin/test -f "$path"',
            '/usr/bin/sudo /bin/test ! -L "$path"',
            '/usr/bin/sudo /usr/bin/shasum -a 256 "$path"',
        ):
            self.assertNotIn(forbidden, continuation_proof)

    def test_marker_freshness_is_early_and_late_checks_bind_exact_state(self) -> None:
        phase = self.job("prepare-slot-1", "resume-slot-1")
        aggregate = WORKFLOW.split("\n  aggregate:\n", 1)[1]
        for required in (
            'test "$marker_age" -le 300',
            'test "$instance_age" -le 300',
            'test "$reboot_age" -le 300',
            'test "$reboot_marker_age" -le 300',
            'run_marker_sha256=$(/usr/bin/sudo -n /usr/bin/shasum -a 256',
            'instance_marker_sha256=$(/usr/bin/sudo -n /usr/bin/shasum -a 256',
            'reboot_marker_sha256=$(/usr/bin/sudo -n /usr/bin/shasum -a 256',
            '"run_id=$GITHUB_RUN_ID"',
            '"boot_uuid=$current_boot"',
            '"reboot_marker_sha256=$reboot_marker_sha256"',
            'reboot_marker_sha256=none',
        ):
            self.assertIn(required, phase)
        for forbidden in ("instance_age=", "reboot_age=", "marker_age=", "stat -f '%m'"):
            self.assertNotIn(forbidden, PROOF)
        for required in (
            '"run_id=$GITHUB_RUN_ID"',
            '"phase=$PKG_PROOF_PHASE"',
            '"boot_uuid=$preflight_boot"',
            '"run_marker_sha256=$preflight_run_marker_sha"',
            '"instance_marker_sha256=$preflight_instance_marker_sha"',
            '"reboot_marker_sha256=$preflight_reboot_marker_sha"',
            '"$(/usr/bin/id -u):600"',
            '[ "$current_boot" = "$preflight_boot" ]',
            '[ "$instance_marker_sha" = "$preflight_instance_marker_sha" ]',
            '[ "$run_marker_sha" = "$preflight_run_marker_sha" ]',
            '[ "$reboot_marker_sha" = "$preflight_reboot_marker_sha" ]',
            "resume:none",
        ):
            self.assertIn(required, PROOF)
        for required in (
            '"run_id": os.environ["GITHUB_RUN_ID"]',
            '"boot_uuid": identity.get("boot_uuid", "")',
            '"run_marker_sha256": identity.get("run_marker_sha256", "")',
            '"instance_marker_sha256": identity.get("instance_marker_sha256", "")',
            '"reboot_marker_sha256": identity.get("reboot_marker_sha256", "")',
            'identity["reboot_marker_sha256"] != "none"',
        ):
            self.assertIn(required, aggregate)

    def test_handoff_base_nix_package_and_service_state_are_byte_compared(self) -> None:
        for name in ("handoff", "base-nix", "package-state", "services"):
            self.assertIn(f'"$work/{name}.before"', PROOF)
            self.assertIn(f'"$work/{name}.after"', PROOF)
            self.assertIn(f'"$continuation_state/{name}.before"', PROOF)
        self.assertIn(
            '/usr/bin/sudo -n /usr/bin/cmp "$continuation_state/$name.before"', PROOF
        )
        self.assertIn("/nix/var/nix/db/db.sqlite", PROOF)
        self.assertIn("org.pkg.root-helper=offline", PROOF)
        self.assertIn("org.pkg.nix-broker=offline", PROOF)
        self.assertIn("external\tn-plus-1-resumed-offline\tpass", PROOF)
        self.assertIn("native\tresume-state-preserved\tpass", PROOF)

    def test_resume_starts_services_only_after_all_resume_checks(self) -> None:
        resume = PROOF.index('if [ "$PKG_PROOF_PHASE" = resume ]')
        verify = PROOF.index("verify_continuation", resume)
        offline = PROOF.index("assert_services_offline", resume)
        compare = PROOF.index("compare_prepare_state", resume)
        start = PROOF.index("start_product", resume)
        self.assertLess(verify, offline)
        self.assertLess(offline, compare)
        self.assertLess(compare, start)

    def test_native_package_remove_checks_generation_behavior(self) -> None:
        for required in (
            'capture package-remove "$pkg" --yes --json remove ripgrep',
            '"$generation_before" != "$generation_after"',
            "did not create and activate a new generation",
            'test -x "$state_root/current/bin/hello"',
            'test ! -e "$state_root/current/bin/rg"',
            'test -f "$state_root/generations/$before_id.json"',
            "native package-remove",
        ):
            self.assertIn(required, PROOF)

    def test_aggregate_requires_four_real_phase_rows(self) -> None:
        aggregate = WORKFLOW.split("\n  aggregate:\n", 1)[1]
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
        ):
            self.assertIn(required, aggregate)
        self.assertNotIn("sudo", aggregate)
        self.assertNotIn("prove.sh", aggregate)

    def test_documentation_describes_the_operator_pause(self) -> None:
        for required in (
            "There is no matrix-order assumption.",
            "Do not destroy or replace the VM.",
            "Reboot the same VM.",
            "Do not change its instance nonce.",
            "Register the same runner name and label",
            "Only then create slot 2.",
            "Do not dispatch the workflow again.",
            "requires exactly four evidence artifacts",
        ):
            self.assertIn(required, README)
        self.assertNotIn("It does not prove product lifecycle recovery across a reboot.", README)

    def test_security_python_is_isolated_and_actions_are_pinned(self) -> None:
        self.assertNotIn("assert ", PROOF)
        self.assertIn("/usr/bin/python3 -I -", PROOF)
        for line in WORKFLOW.splitlines():
            if "uses:" in line and "./" not in line:
                revision = line.rsplit("@", 1)[-1]
                self.assertEqual(len(revision), 40)
                int(revision, 16)


if __name__ == "__main__":
    unittest.main()
