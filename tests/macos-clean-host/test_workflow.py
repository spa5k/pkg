"""Structural safety checks for the destructive macOS lifecycle proof."""

import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = Path(
    os.environ.get("PKG_MACOS_PROOF_WORKFLOW", ROOT / ".github/workflows/macos-alpha-proof.yml")
)
WORKFLOW = WORKFLOW_PATH.read_text()
NIGHTLY = (WORKFLOW_PATH.parent / "nightly.yml").read_text()
PROOF = (ROOT / "tests/macos-clean-host/prove.sh").read_text()
README = (ROOT / "tests/macos-clean-host/README.md").read_text()
INSTALL_FAILURE_PARSER = PROOF.split(
    '        0 0 >"$summary" <<\'PY\'\n', 1
)[1].split("\nPY\n", 1)[0]
CAPACITY_PARSER = textwrap.dedent(
    WORKFLOW.split(
        '          python3 -I - 75161927680 "$root_capacity" <<\'PY\'\n', 1
    )[1].split("\n          PY\n", 1)[0]
)


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
            'for digest in "$PKG_PROOF_PAIR_SHA256"',
            'for count in "$PKG_PROOF_PAIR_LENGTH"',
            '[[ "$digest" =~ ^[0-9a-f]{64}$ ]]',
            '[[ "$count" =~ ^[1-9][0-9]*$ ]]',
        ):
            self.assertIn(required, validate)
        self.assertLess(
            validate.index('[[ "$count" =~ ^[1-9][0-9]*$ ]]'),
            validate.index('gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/'),
        )
        self.assertIn("PKG_PROOF_WORKFLOW_TAG: dn16-macos-proof-workflow-18", WORKFLOW)
        self.assertIn(
            "PKG_REVIEWED_COMMIT: 4de8b127d46785fbb86a1aab957a5b2e27737a8e",
            WORKFLOW,
        )
        for final_value in (
            "PKG_PROOF_PAIR_SHA256: fcc8b01a4c1a76290a400fbae8c798c8b9298c7722aa1454670f5dc7d4c68f42",
            "PKG_PROOF_PAIR_LENGTH: 1101",
            "PKG_PROOF_N_INVENTORY_SHA256: a408156afa18acd5b84926a967b5b5e22e26fca650508a273dbfba3c9fe05855",
            "PKG_PROOF_N_INVENTORY_LENGTH: 5959",
            "PKG_PROOF_N_PLUS_1_INVENTORY_SHA256: 14e4d65af7f195679313ee6dc35fd882fab1d1df80b4c547f1087879d296bf0a",
            "PKG_PROOF_N_PLUS_1_INVENTORY_LENGTH: 5959",
            "PKG_PROOF_N_TOTAL_BYTES: 328999083",
            "PKG_PROOF_N_ROWS_SHA256: 2430491fbe894677fe226becb4b10a9abfe9e9256754ac90200e09ea91d5e26b",
            "PKG_PROOF_N_PLUS_1_TOTAL_BYTES: 328600801",
            "PKG_PROOF_N_PLUS_1_ROWS_SHA256: 58ed6fd61a6fcb962ee15fcf67e75a29637dfd7babfa617fa8d3c9b65fefc359",
            "PKG_PROOF_INPUT_BYTES: 35957011",
            "PKG_PROOF_RESPONSE_BYTES: 35970030",
        ):
            self.assertEqual(WORKFLOW.count(final_value), 1)
        self.assertNotIn("REPLACE_WITH_FINAL", WORKFLOW)
        for stale_value in (
            "dn16-macos-proof-workflow-17",
            "dn16-macos-proof-workflow-16",
            "118f41dde97de3825a91a395f39f8094e42ffc86",
            "35088d4fa25cd827f641d28f63f6caa0d72fa031be6fc10ee842d0fe0c16962f",
            "2def0fd1f2f64cca46c02c102514ebaa18c19d9f2becf9e9d20e0aa366f39381",
            "8c0acc759f1361aca19efdbacb93ff48c8ac6b93826a6ad67d57f5a938b749bf",
            "ec28d9b3ef2ec9675d2a8597be0e92c16758b87d5a324ff04e45d8200d4dcac3",
            "db47151479caf4474fc30df7d22833ac46acd444d90c395f6dbbe2a76f748ffb",
        ):
            self.assertNotIn(stale_value, WORKFLOW)
        self.assertIn('test "$FROM_RELEASE" = v0.1.0-alpha.20', WORKFLOW)
        self.assertIn('test "$TO_RELEASE" = v0.1.0-alpha.21', WORKFLOW)
        self.assertIn(
            '[ "$PKG_PROOF_FROM_RELEASE" = v0.1.0-alpha.20 ]', PROOF
        )
        self.assertIn(
            '[ "$PKG_PROOF_TO_RELEASE" = v0.1.0-alpha.21 ]', PROOF
        )

    def test_persistent_handoff_lock_has_exact_residue_metadata(self) -> None:
        self.assertIn("/private/var/db/pkg-install-handoff.lock", PROOF)
        self.assertIn("/usr/bin/sudo -n /usr/bin/stat -f", PROOF)
        self.assertIn("Regular File:root:wheel:600:0:1", PROOF)
        self.assertIn("the persistent handoff lock metadata changed", PROOF)
        self.assertNotIn("/private/var/run/pkg-install-handoff.lock", PROOF)

    def test_guest_capacity_gate_is_early_exact_and_fail_closed(self) -> None:
        phase = self.job("prepare-slot-1", "resume-slot-1")
        gate = phase.split(
            "      - name: Refuse an unsafe host before input download or mutation\n", 1
        )[1].split("\n      - name: Download proof-only harness", 1)[0]
        self.assertIn("root_capacity=$(LC_ALL=C /bin/df -Pk /)", gate)
        self.assertIn('python3 -I - 75161927680 "$root_capacity"', gate)
        self.assertLess(gate.index("/bin/df -Pk /"), gate.index("preflight_time=$(date +%s)"))
        self.assertIn("75,161,927,680 free bytes (70 GiB)", README)
        self.assertIn("at least 100 GiB", README)

        def parse(
            available: str,
            *,
            blocks: str = "100000000",
            used: str = "1",
            capacity: str = "1%",
            header: str = "Filesystem 1024-blocks Used Available Capacity Mounted on",
        ) -> subprocess.CompletedProcess[str]:
            report = (
                f"{header}\n"
                f"/dev/disk3s1s1 {blocks} {used} {available} {capacity} /\n"
            )
            return subprocess.run(
                [sys.executable, "-I", "-", "75161927680", report],
                input=CAPACITY_PARSER,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertNotEqual(parse("73400319").returncode, 0)
        self.assertEqual(parse("73400320").returncode, 0)
        self.assertEqual(parse("73400321").returncode, 0)
        for malformed in ("", "-1", "not-a-number", str(2**63 // 1024)):
            self.assertNotEqual(parse(malformed).returncode, 0)
        self.assertNotEqual(parse("73400320", blocks="invalid").returncode, 0)
        self.assertNotEqual(parse("73400320", capacity="invalid").returncode, 0)
        self.assertNotEqual(parse("73400320", header="invalid").returncode, 0)

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
            "pkg-macos-hosted-acquisition-receipt",
            "PKG-DN16-HOSTED-ACQUISITION-V1",
            "if len(rows) != 68",
            'f"{prefix}_verified_count={len(channel_rows)}"',
            'values.append("status=complete")',
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
        self.assertNotIn("pkg-macos-authenticated-inputs", WORKFLOW)

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
            "\n      - name: Verify the provisioned Sigstore verifier", 1
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

    def run_install_failure_parser(
        self,
        handoff: Path,
        journal: Path,
        *,
        status: int = 1,
        owner: int | None = None,
        group: int | None = None,
    ) -> str:
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-",
                str(status),
                str(handoff),
                str(journal),
                str(os.getuid() if owner is None else owner),
                str(os.getgid() if group is None else group),
            ],
            input=INSTALL_FAILURE_PARSER,
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertEqual(result.stderr, "")
        self.assertLessEqual(len(result.stdout.encode()), 1024)
        return result.stdout

    @staticmethod
    def write_private(path: Path, value: object | bytes) -> None:
        raw = value if isinstance(value, bytes) else json.dumps(value).encode()
        path.write_bytes(raw)
        path.chmod(stat.S_IRUSR | stat.S_IWUSR)

    @staticmethod
    def handoff(state: str) -> dict[str, object]:
        value: dict[str, object] = {"kind": state}
        if state == "accepted":
            identity = {"length": 1, "sha256": "sha256-" + "a" * 64}
            value |= {"installer": identity, "receipt": identity}
        return {"schema_version": 1, "state": value}

    def test_fresh_install_failure_summary_has_exact_bounded_schema(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = self.run_install_failure_parser(root / "handoff", root / "journal", status=17)
        self.assertEqual(
            output,
            "installer_status=17\nhandoff_state=absent\njournal_present=false\n",
        )
        failure = PROOF.split("summarize_fresh_install_failure() {", 1)[1].split(
            "capture_fresh_install() {", 1
        )[0]
        for required in (
            'summary="$evidence/fresh-install-summary.txt"',
            "/private/var/db/pkg-install/determinate-handoff-v1.json",
            "/private/var/db/pkg-install-journal/macos-transaction-v1.json",
            "/usr/bin/sudo -n /usr/bin/python3 -I -",
            "64 * 1024",
            "32 * 1024",
            'stat.S_ISREG(metadata.st_mode)',
            'stat.S_IMODE(metadata.st_mode) == 0o600',
            'metadata.st_uid == owner',
            'metadata.st_gid == group',
            'metadata.st_nlink == 1',
            'valid_private(before, limit, owner, group)',
            'valid_private(opened, limit, owner, group)',
            'valid_private(after, limit, owner, group)',
            'valid_private(current, limit, owner, group)',
            'set(record) != {"schema_version", "state"}',
            "set(state) == {\"kind\"}",
            "set(state) == {\"kind\", \"installer\", \"receipt\"}",
            'handoff_state = "invalid"',
            'journal_present = True',
        ):
            self.assertIn(required, failure)
        self.assertIn("-le 1024", failure)
        self.assertIn("-eq 3", failure)
        self.assertIn("installer_status=$summary_status", failure)
        self.assertIn("handoff_state=$summary_handoff", failure)
        self.assertIn("journal_present=$summary_journal", failure)

    def test_fresh_install_failure_classifies_valid_protected_state(self) -> None:
        cases = (("started", False), ("accepted", True))
        for state, journal_present in cases:
            with self.subTest(state=state, journal_present=journal_present):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    handoff = root / "handoff"
                    journal = root / "journal"
                    self.write_private(handoff, self.handoff(state))
                    if journal_present:
                        self.write_private(journal, b"bounded private journal")
                    output = self.run_install_failure_parser(handoff, journal)
                self.assertEqual(
                    output,
                    f"installer_status=1\nhandoff_state={state}\n"
                    f"journal_present={'true' if journal_present else 'false'}\n",
                )

    def test_fresh_install_failure_rejects_unsafe_or_malformed_state(self) -> None:
        cases = {
            "malformed": b"{",
            "unknown-field": {
                "schema_version": 1,
                "state": {"kind": "started"},
                "extra": True,
            },
            "oversize": b" " * (64 * 1024 + 1),
        }
        for name, value in cases.items():
            with self.subTest(name=name):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    handoff = root / "handoff"
                    self.write_private(handoff, value)
                    output = self.run_install_failure_parser(handoff, root / "journal")
                self.assertIn("handoff_state=invalid\n", output)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            self.write_private(target, self.handoff("started"))
            (root / "handoff").symlink_to(target)
            output = self.run_install_failure_parser(root / "handoff", root / "journal")
        self.assertIn("handoff_state=invalid\n", output)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            handoff = root / "handoff"
            self.write_private(handoff, self.handoff("started"))
            handoff.chmod(0o644)
            output = self.run_install_failure_parser(handoff, root / "journal")
        self.assertIn("handoff_state=invalid\n", output)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            handoff = root / "handoff"
            self.write_private(handoff, self.handoff("started"))
            output = self.run_install_failure_parser(
                handoff, root / "journal", owner=os.getuid() + 1
            )
        self.assertIn("handoff_state=invalid\n", output)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            handoff = root / "handoff"
            self.write_private(handoff, self.handoff("started"))
            output = self.run_install_failure_parser(
                handoff, root / "journal", group=os.getgid() + 1
            )
        self.assertIn("handoff_state=invalid\n", output)

    def test_unsafe_journal_is_present_but_invalidates_the_state_class(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            handoff = root / "handoff"
            journal = root / "journal"
            self.write_private(handoff, self.handoff("accepted"))
            self.write_private(journal, b"x" * (32 * 1024 + 1))
            output = self.run_install_failure_parser(handoff, journal)
        self.assertEqual(
            output,
            "installer_status=1\nhandoff_state=invalid\njournal_present=true\n",
        )

    def test_fresh_install_dumplog_keeps_bounded_capture_and_private_summary(self) -> None:
        install = PROOF.split("capture_fresh_install() {", 1)[1].split(
            'echo "+ clean install from signed release N"', 1
        )[0]
        command = (
            '/usr/bin/sudo /usr/sbin/installer -dumplog -pkg "$from_pkg" -target /'
        )
        self.assertEqual(install.count(command), 1)
        self.assertIn(
            '/usr/bin/tail -c 65536 "$work/fresh-install.log" '
            '>"$evidence/fresh-install.log"',
            install,
        )
        self.assertLess(install.index("[ \"$status\" -eq 0 ]"), install.index(
            'summarize_fresh_install_failure "$status"'
        ))
        diagnostic = PROOF.split("summarize_fresh_install_failure() {", 1)[1].split(
            "assert_accepted() {", 1
        )[0]
        for forbidden in (
            "/var/log/install.log",
            "/nix/receipt.json",
            "/bin/cp",
            "shutil",
            "os.environ",
            "print(record",
            "print(raw",
            '"$evidence/determinate-handoff',
            '"$evidence/macos-transaction',
        ):
            self.assertNotIn(forbidden, diagnostic)

    def test_vm_uses_the_exact_provisioned_sigstore_verifier(self) -> None:
        hosted = self.job("acquire-inputs", "prepare-slot-1")
        phase = self.job("prepare-slot-1", "resume-slot-1")
        verifier = phase.split(
            "      - name: Verify the provisioned Sigstore verifier\n", 1
        )[1].split("\n      - name: Acquire compact authenticated proof inputs", 1)[0]
        self.assertIn(
            "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6",
            hosted,
        )
        self.assertNotIn("sigstore/cosign-installer", phase)
        for required in (
            "cosign=/usr/local/bin/cosign",
            'test -f "$cosign"',
            'test ! -L "$cosign"',
            'test -x "$cosign"',
            'test "$(/usr/bin/stat -f \'%Su:%Sg:%Lp\' "$cosign")" = root:wheel:755',
            "77bbab240111761d50044f37541da0734d964dfe5f092cab6d584663c912372e",
            'version=$("$cosign" version)',
            "')\" = v3.1.3",
        ):
            self.assertIn(required, verifier)
        compact = phase.split(
            "      - name: Acquire compact authenticated proof inputs\n", 1
        )[1].split("\n      - name: Run the selected proof phase", 1)[0]
        self.assertEqual(compact.count("/usr/local/bin/cosign verify-blob"), 2)
        self.assertNotIn("\n            cosign verify-blob", compact)

    def test_vm_acquisition_is_compact_atomic_and_fully_authenticated(self) -> None:
        phase = self.job("prepare-slot-1", "resume-slot-1")
        compact = phase.split("      - name: Acquire compact authenticated proof inputs\n", 1)[
            1
        ].split("\n      - name: Run the selected proof phase", 1)[0]
        for required in (
            "--proto '=https'",
            "--tlsv1.2",
            "--retry 5",
            "--retry-all-errors",
            'temporary="$output.part"',
            'test "$response" = "200 $url"',
            'mv "$temporary" "$output"',
            '"$PKG_PROOF_N_INVENTORY_LENGTH" "$PKG_PROOF_N_INVENTORY_SHA256"',
            '"$PKG_PROOF_N_PLUS_1_INVENTORY_SHA256"',
            "require(len(selected_rows) == 18",
            "proof_input_bytes = sum(row[2] for row in selected_rows)",
            'proof_input_bytes == int(os.environ["PKG_PROOF_INPUT_BYTES"])',
            'int(os.environ["PKG_PROOF_RESPONSE_BYTES"])',
            '== proof_inputs, "compact proof inputs are missing or extra"',
            'cosign verify-blob --bundle "$dir/SHA256SUMS.sigstore.json"',
            '--certificate-identity "$identity" --certificate-oidc-issuer "$issuer"',
            'for asset in "pkg-$version-preview.pkg" pkg-aarch64-darwin',
            'manifest.get("releaseId") != sys.argv[2]',
        ):
            self.assertIn(required, compact)
        self.assertNotIn("--location", compact)
        self.assertNotIn("actions/download-artifact", compact)
        self.assertNotIn("pkg-macos-authenticated-inputs", phase)
        self.assertIn('from="$channel/n/proof-inputs"', PROOF)
        self.assertIn('to="$channel/n-plus-1/proof-inputs"', PROOF)
        self.assertIn("actual == proof_inputs", PROOF)
        self.assertNotIn("actual == set(files)", PROOF)
        for required in (
            "schema=PKG-DN16-VM-ACQUISITION-V1",
            "logical_fetches=21",
            "proof_input_bytes=$PKG_PROOF_INPUT_BYTES",
            "response_bytes=$PKG_PROOF_RESPONSE_BYTES",
            '"proof_pair_sha256": os.environ["PROOF_PAIR_SHA256"]',
            'raise SystemExit("VM acquisition evidence does not match its phase")',
        ):
            self.assertIn(required, WORKFLOW)
        self.assertIn("the VM acquisition evidence does not bind this job", PROOF)

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
