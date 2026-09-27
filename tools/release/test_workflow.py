"""Structural release-workflow security contract."""

from pathlib import Path
import os
import re
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
LINUX_HARNESS = (ROOT / "tests/linux-clean-host/run.sh").read_text(encoding="utf-8")
LINUX_STAGE = (ROOT / "tests/linux-clean-host/Dockerfile.stage").read_text(
    encoding="utf-8"
)
LINUX_HOST = (ROOT / "tests/linux-clean-host/Dockerfile").read_text(encoding="utf-8")
PROOF_PUBLICATION = (
    ROOT / "tools/release/examples/linux_proof_publication.rs"
).read_text(encoding="utf-8")
PROOF_SERVER = (ROOT / "tools/release/serve_proof_channel.py").read_text(
    encoding="utf-8"
)
MACOS_WORKFLOW = (ROOT / ".github/workflows/macos-alpha-proof.yml").read_text(
    encoding="utf-8"
)
LINUX_RUN_SH = ROOT / "tests/linux-clean-host/run.sh"


class ReleaseWorkflowTests(unittest.TestCase):
    def test_alpha_build_includes_the_native_catalog_builder(self) -> None:
        workflow = (ROOT / ".github/workflows/alpha-release.yml").read_text()
        self.assertIn("-p pkg-release --bin pkg-release-index", workflow)
        self.assertIn("pkg-root-helper pkg-install pkg-release-index; do", workflow)
        self.assertIn('for artifact in assets/*; do', workflow)

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
        candidate = "pkg-v0.1.0-alpha.57-linux-x86_64.tar.gz"
        self.assertIn(f"proof-artifacts/{candidate}", WORKFLOW)
        self.assertIn("pkg-v0.1.0-alpha.57-linux-x86_64-candidate", WORKFLOW)
        self.assertIn("proof-artifacts/evidence/", WORKFLOW)
        self.assertIn("pkg-v0.1.0-alpha.57-x86_64-linux-proof", WORKFLOW)
        self.assertIn("retention-days: 7", WORKFLOW)
        self.assertIn("set -o pipefail", WORKFLOW)
        self.assertIn('tee "$RUNNER_TEMP/dn15-runtime.log"', WORKFLOW)
        self.assertIn("proof-artifacts/evidence/dn15-runtime.log", WORKFLOW)
        self.assertIn("if: ${{ always() }}", WORKFLOW)
        self.assertIn("if: ${{ success() }}", WORKFLOW)
        self.assertIn(
            "- name: Retain the candidate without publishing it\n"
            "        if: ${{ success() }}",
            WORKFLOW,
        )
        self.assertIn(
            "- name: Retain the proof evidence without publishing it\n"
            "        if: ${{ always() }}",
            WORKFLOW,
        )

    def _single_block(self, lines: list[str], header: str) -> list[str]:
        """Return the block opened by exactly one `header` line; fail closed."""
        matches = [index for index, line in enumerate(lines) if line == header]
        self.assertEqual(
            len(matches),
            1,
            f"expected exactly one {header!r} block, found {len(matches)}",
        )
        indent = " " * (len(header) - len(header.lstrip()) + 1)
        start = matches[0]
        end = len(lines)
        for index in range(start + 1, len(lines)):
            line = lines[index]
            if line.strip() and not line.startswith(indent):
                end = index
                break
        return lines[start:end]

    def test_production_linux_input_is_manual_fixed_and_not_published(self) -> None:
        lines = WORKFLOW.splitlines()
        permissions = self._single_block(lines, "permissions:")
        self.assertEqual(
            [line for line in permissions[1:] if line.strip()], ["  contents: read"]
        )
        on_block = self._single_block(lines, "on:")
        dispatch = self._single_block(on_block, "  workflow_dispatch:")
        inputs = self._single_block(dispatch, "    inputs:")
        production_input = self._single_block(inputs, "      production-linux:")
        for key, value in (("required", "false"), ("type", "boolean"), ("default", "false")):
            self.assertEqual(
                [line for line in production_input if line.startswith(f"        {key}:")],
                [f"        {key}: {value}"],
            )
        jobs = self._single_block(lines, "jobs:")
        production_job = self._single_block(jobs, "  production-linux:")
        self.assertEqual(
            [line for line in production_job if line.startswith("    if:")],
            [
                "    if: ${{ github.event_name == 'workflow_dispatch' "
                "&& inputs.production-linux }}"
            ],
        )
        self.assertEqual(
            [line for line in production_job if line.startswith("    runs-on:")],
            ["    runs-on: ubuntu-22.04"],
        )
        self.assertFalse(any(line.startswith("    permissions:") for line in production_job))
        for gate_job in ("dry-run-sign", "linux-alpha-proof"):
            gate = self._single_block(jobs, f"  {gate_job}:")
            self.assertEqual(
                [line for line in gate if line.startswith("    if:")],
                [
                    "    if: ${{ github.event_name != 'workflow_dispatch' "
                    "|| !inputs.production-linux }}"
                ],
            )
        production_source = "\n".join(production_job)
        self.assertIn("https://releases.happytoolin.com/metadata/1.root.json", production_source)
        self.assertIn(
            "52523a9bf76dee8e364efc302b733140f850fe377c1cc73a7675b842d28b94e2",
            production_source,
        )
        self.assertIn("pkg-v0.1.0-alpha.57-production-linux-input", production_source)
        self.assertIn("pkg-release-index", production_source)

    def test_linux_uninstall_uses_plain_terminal_exec_status(self) -> None:
        self.assertEqual(
            LINUX_HARNESS.count(
                'docker exec "$container" env PKG_INSTALL_DEBUG=1 '
                '/usr/local/bin/pkg --yes system uninstall'
            ),
            1,
        )
        self.assertIn("uninstall_status=$?", LINUX_HARNESS)
        self.assertIn('exit "$uninstall_status"', LINUX_HARNESS)
        self.assertIn("flag=--json", LINUX_HARNESS)
        self.assertIn("flag=--jsonl", LINUX_HARNESS)
        self.assertEqual(
            LINUX_HARNESS.count(
                'docker exec "$container" /usr/local/bin/pkg "$flag" --yes system uninstall'
            ),
            1,
        )
        self.assertIn('test "$status" -eq 78', LINUX_HARNESS)
        self.assertIn('test ! -s "$stderr"', LINUX_HARNESS)
        self.assertIn('cmp "$before" "$after"', LINUX_HARNESS)

    def test_linux_proof_binary_is_isolated_and_every_blocker_runs_twice(self) -> None:
        build = (
            "cargo test --locked --release \\\n"
            "        --target x86_64-unknown-linux-gnu \\\n"
            "        -p pkg-installer --lib --no-run --message-format=json"
        )
        self.assertEqual(LINUX_STAGE.count(build), 1)
        self.assertIn(
            'COPY test-binaries/pkg-installer-lib-tests '
            '/usr/local/libexec/pkg-installer-lib-tests',
            LINUX_HOST,
        )
        self.assertIn('cp -a "$raw_stage/test-binaries" "$artifact_context/"', LINUX_HARNESS)
        self.assertGreater(
            LINUX_HARNESS.index('package_alpha_candidate.py'),
            LINUX_HARNESS.index('docker build'),
        )
        self.assertLess(
            LINUX_HARNESS.index('package_alpha_candidate.py'),
            LINUX_HARNESS.index('cp -a "$raw_stage/test-binaries"'),
        )
        self.assertIn('test_binary_sha256', LINUX_HARNESS)
        self.assertIn("meta\\tsigned_commit", LINUX_HARNESS)
        self.assertIn("meta\\tdocker_server_arch", LINUX_HARNESS)
        self.assertIn("file /usr/local/libexec/pkg-installer-lib-tests", LINUX_HARNESS)
        self.assertIn(
            "readelf --file-header /usr/local/libexec/pkg-installer-lib-tests",
            LINUX_HARNESS,
        )
        self.assertIn("ldd /usr/local/libexec/pkg-installer-lib-tests", LINUX_HARNESS)
        for case in (
            "persisted-started-refusal",
            "structured-json",
            "structured-jsonl",
            "sync-exec-restore",
            "sync-exec-restore-failure",
            "post-unlink-clear-restore",
            "real-sigkill-unmarked",
            "later-outcome-unknown",
            "vendor-action-last",
            "install-process-controls",
            "product-upgrade",
            "product-asset-repair",
            "package-operations",
            "package-repair",
            "package-roots-gc",
            "old-runtime-absent",
            "changed-vendor-leaf",
            "terminal-uninstall",
            "same-host-reinstall",
        ):
            self.assertIn(case, LINUX_HARNESS)
        self.assertIn('"$results")" -eq 38', LINUX_HARNESS)
        self.assertIn(
            "test ! -e /opt/pkg/nix\n    test ! -L /opt/pkg/nix", LINUX_HARNESS
        )
        for evidence in (
            "docker-inspect.json",
            "docker.log",
            "final-state.txt",
            "residue.txt",
        ):
            self.assertIn(evidence, LINUX_HARNESS)
        cleanup = LINUX_HARNESS.split("cleanup() {\n", 1)[1].split("\n}\n", 1)[0]
        self.assertLess(
            cleanup.index('capture_failure "$status"'), cleanup.index("stop_container")
        )
        self.assertLess(
            LINUX_HARNESS.index('mkdir -p -m 0700 "$artifact_output/evidence"'),
            LINUX_HARNESS.index('echo "+ stage x86_64 Linux release inputs"'),
        )
        self.assertIn(
            "--legacy-linux-fixture /publication-1 /runtime /binaries-n",
            LINUX_STAGE,
        )
        self.assertIn(
            "--legacy-linux-fixture /publication-2 /runtime /binaries-n-plus-1",
            LINUX_STAGE,
        )
        self.assertIn(
            "copy_sigstore_bundle(artifact_root, &bundle, &bundle_input)",
            PROOF_PUBLICATION,
        )
        self.assertIn("dn16_refuses_placeholder_and_plain_text_bundles", PROOF_PUBLICATION)
        for command in ("--prepare-dn16-manifest", "--publish-dn16"):
            self.assertIn(command, PROOF_PUBLICATION)
        for mode in ("Dn16Prepared", "Dn16Sealed", "LegacyLinuxFixture"):
            self.assertIn(mode, PROOF_PUBLICATION)
        self.assertIn("ReleaseManifest::from_prepared_json", PROOF_PUBLICATION)
        self.assertNotIn("ProofInputMode::Dn16Fixture", PROOF_PUBLICATION)
        self.assertIn('"bootstrap": (3, bootstrap)', PROOF_SERVER)
        self.assertIn('"activate": (2, activate)', PROOF_SERVER)
        self.assertNotIn('"start":', PROOF_SERVER)
        self.assertIn("--bind-dn16-pair", PROOF_PUBLICATION)
        self.assertIn("validate_pair(staging)", PROOF_SERVER)
        self.assertIn("verify_remote(state[\"url\"], records)", PROOF_SERVER)
        self.assertIn('state["phase"] = "active"', PROOF_SERVER)
        self.assertLess(
            PROOF_SERVER.index("verify_remote(state[\"url\"], records)"),
            PROOF_SERVER.index('state["phase"] = "active"'),
        )
        self.assertIn("$name.sigstore.json", LINUX_STAGE)
        for name in (
            "pkg-aarch64-darwin",
            "pkg-x86_64-linux",
            "pkg-installer-x86_64-linux",
        ):
            self.assertIn(name, LINUX_STAGE)
        self.assertIn(
            "PKG_RELEASE_CHANNEL_METADATA_URL=https://127.0.0.1:8443/metadata/./",
            LINUX_STAGE,
        )
        self.assertIn("assert_publication_product /srv/pkg-releases/2", LINUX_HARNESS)
        self.assertIn("--repair-product-assets", LINUX_HARNESS)
        self.assertIn("cmp \"$product_evidence/repair-active-before.json\"", LINUX_HARNESS)
        self.assertIn(
            'printf "damaged broker service\\n" > '
            "/usr/lib/systemd/system/pkg-nix-broker.service",
            LINUX_HARNESS,
        )
        self.assertIn(
            'sha256sum /usr/lib/systemd/system/pkg-nix-broker.service',
            LINUX_HARNESS,
        )
        activation = LINUX_HARNESS.split("activate_product_units() {\n", 1)[1].split(
            "\n}\n", 1
        )[0]
        self.assertLess(
            activation.index('assert_publication_product "$1"'),
            activation.index("systemctl daemon-reload"),
        )
        receipt_files = LINUX_HARNESS.split("file_paths = {\n", 1)[1].split(
            "\n}\n", 1
        )[0]
        receipt_records = LINUX_HARNESS.split("expected_records = {\n", 1)[1].split(
            "\n}\n", 1
        )[0]
        self.assertEqual(len(set(re.findall(r'"([a-z0-9-]+)"', receipt_records))), 32)
        self.assertIn("records.keys() != expected_records", LINUX_HARNESS)
        for asset in (
            "root-helper-binary",
            "broker-binary",
            "nix-config",
            "helper-socket-unit",
            "helper-service-unit",
            "broker-socket-unit",
            "broker-service-unit",
            "runtime-tmpfiles",
            "profile-snippet",
            "product-cli",
        ):
            self.assertIn(f'"{asset}"', receipt_files)
        self.assertIn(
            'records[asset].get("contentDigest") != receipt_digest(actual)',
            LINUX_HARNESS,
        )
        self.assertIn("path.resolve(strict=True)", LINUX_HARNESS)
        self.assertIn('print("gc-root\\t"', LINUX_HARNESS)
        active_repair = LINUX_HARNESS.split(
            'echo "+ active product repair refusal without mutation"', 1
        )[1].split('echo "+ authenticated offline product asset repair"', 1)[0]
        active_sequence = """snapshot_package_state "$product_evidence/package-state-before-active-repair-refusal.txt"
set +e
docker exec "$container" "$n_plus_1_installer" --repair-product-assets \\
    > "$product_evidence/repair-active.stdout" \\
    2> "$product_evidence/repair-active.stderr"
repair_active_status=$?
set -e
snapshot_package_state "$product_evidence/package-state-after-active-repair-refusal.txt"
cmp "$product_evidence/package-state-before-active-repair-refusal.txt" \\
    "$product_evidence/package-state-after-active-repair-refusal.txt"""
        self.assertIn(active_sequence, active_repair)

        offline_repair = LINUX_HARNESS.split(
            'echo "+ authenticated offline product asset repair"', 1
        )[1].split('echo "+ activate verified repaired N+1 product services"', 1)[0]
        offline_sequence = """snapshot_package_state "$product_evidence/package-state-before-offline-repair.txt"
repair_output=$(docker exec "$container" "$n_plus_1_installer" --repair-product-assets)
snapshot_package_state "$product_evidence/package-state-after-repair.txt"
cmp "$product_evidence/package-state-before-offline-repair.txt" \\
    "$product_evidence/package-state-after-repair.txt"""
        self.assertIn(offline_sequence, offline_repair)

        active_before = (
            'snapshot_package_state '
            '"$product_evidence/package-state-before-active-repair-refusal.txt"'
        )
        upgrade_compare = (
            'cmp "$product_evidence/package-state-before.txt" '
            '\\\n    "$product_evidence/package-state-after-upgrade.txt"'
        )
        intervening_list = (
            'docker exec "$container" su - proof-user -c '
            '"/usr/local/bin/pkg --json list"'
        )
        upgrade_compare_index = LINUX_HARNESS.index(upgrade_compare)
        intervening_list_index = LINUX_HARNESS.index(
            intervening_list, upgrade_compare_index
        )
        self.assertLess(upgrade_compare_index, intervening_list_index)
        self.assertLess(intervening_list_index, LINUX_HARNESS.index(active_before))
        self.assertEqual(LINUX_HARNESS.count("run_filter_group product-upgrade"), 1)
        self.assertEqual(
            LINUX_HARNESS.count("run_filter_group product-asset-repair"), 1
        )
        self.assertGreaterEqual(LINUX_HARNESS.count("assert_product_units_offline"), 3)
        self.assertLess(
            LINUX_HARNESS.index("assert_publication_product /srv/pkg-releases/2"),
            LINUX_HARNESS.index('echo "+ activate verified N+1 product services"'),
        )
        self.assertLess(
            LINUX_HARNESS.index("package-state-after-upgrade.txt"),
            LINUX_HARNESS.index("run_filter_group product-upgrade"),
        )
        self.assertLess(
            LINUX_HARNESS.index("package-state-after-repair.txt"),
            LINUX_HARNESS.index("run_filter_group product-asset-repair"),
        )
        service_digest = (
            'test "$(docker exec "$container" sha256sum '
            '/usr/lib/systemd/system/pkg-nix-broker.service | awk \'{print $1}\')" '
            '= "$repair_service"'
        )
        self.assertIn(service_digest, LINUX_HARNESS)
        self.assertLess(
            LINUX_HARNESS.index(service_digest),
            LINUX_HARNESS.index("run_filter_group product-asset-repair"),
        )

        block = LINUX_HARNESS.split('cat > "$filters" <<\'EOF\'\n', 1)[1].split(
            "\nEOF\n", 1
        )[0]
        filters = [line.split("\t", 1)[1] for line in block.splitlines()]
        expected_filters = {
            "linux_backend::tests::production_preflight_refuses_persisted_started_without_later_mutation",
            "bootstrap::tests::started_handoff_preflight_prevents_product_mutation_and_vendor_start",
            "determinate_handoff::tests::handoff_record_is_atomic_private_strict_and_contains_no_receipt_data",
            "determinate_handoff::tests::synchronous_exec_error_restores_exact_accepted_handoff",
            "determinate_handoff::tests::synchronous_exec_and_restore_failure_is_fail_closed",
            "determinate_handoff::tests::every_post_unlink_clear_failure_restores_exact_accepted_handoff",
            "determinate_handoff::tests::sigkill_after_consume_leaves_unmarked_determinate_state_for_install_refusal",
            "determinate_handoff::tests::sigkill_after_vendor_exec_keeps_later_outcome_unknown_and_refuses_retry",
            "determinate_handoff::tests::terminal_uninstall_consumes_handoff_only_after_identity_revalidation",
            "uninstall::tests::linux_vendor_uninstall_is_the_terminal_action",
            "uninstall::tests::service_stop_is_a_cleanup_barrier",
            "uninstall::tests::cleanup_failures_do_not_skip_residue_verification",
            "uninstall::tests::product_cleanup_failure_never_dispatches_terminal_vendor",
            "uninstall::tests::residue_failure_has_priority_and_success_is_total",
            "determinate::tests::operations_use_exact_argv_and_cleared_environment",
            "determinate::tests::terminal_uninstall_uses_exact_fixed_argv_and_environment",
            "determinate::tests::executable_authentication_rejects_every_invalid_shape",
            "determinate::tests::both_large_streams_are_drained_and_capped",
            "determinate::tests::exit_nonzero_and_signal_are_distinct",
            "determinate::tests::late_success_is_not_reclassified_as_failure",
            "determinate::tests::synchronous_supervisor_reaps_child_before_return",
            "determinate::tests::diagnostics_never_expose_captured_bytes_or_paths",
            "bootstrap::tests::only_exit_zero_is_vendor_success",
            "determinate::tests::spawn_failure_is_reported_without_terminal_outcome",
            "determinate::tests::wait_failure_is_reported_after_one_vendor_start",
            "bootstrap::tests::nonzero_exit_preserves_started_and_refuses_retry",
            "bootstrap::tests::signal_preserves_started_and_refuses_retry",
            "bootstrap::tests::real_supervisor_loss_preserves_started_and_refuses_second_start",
            "bootstrap::tests::crash_before_vendor_start_preserves_started_and_refuses_retry",
            "bootstrap::tests::crash_after_exit_zero_before_acceptance_preserves_started",
            "bootstrap::tests::failed_installed_state_validation_preserves_started",
            "bootstrap::tests::exit_zero_plus_installed_state_validation_accepts_handoff_exactly_once",
            "bootstrap::tests::spawn_and_wait_uncertainty_preserves_started_and_refuses_retry",
            "bootstrap::tests::failed_product_receipt_publication_keeps_accepted_handoff",
            "bootstrap::tests::journaled_existing_product_update_stays_offline_and_never_starts_determinate",
            "bootstrap::tests::offline_state_change_blocks_the_next_file_mutation_and_rollback",
            "bootstrap::tests::failed_existing_product_update_restores_files_and_stays_offline",
            "platform::linux::assets::tests::ordinary_upgrade_requires_different_release_and_prior_content_identity",
            "linux_filesystem::tests::upgrade_replaces_only_exact_prior_owned_bytes_and_rolls_back",
            "bootstrap::tests::journaled_offline_repair_changes_product_files_without_service_mutation",
            "bootstrap::tests::journaled_repair_refuses_non_offline_service_state_before_mutation",
            "bootstrap::tests::failed_offline_repair_rolls_forward_files_without_service_mutation",
            "linux_systemd::tests::offline_preflight_is_query_only_and_refuses_every_non_offline_state",
            "platform::linux::assets::tests::repair_requires_same_release_and_created_product_ownership",
            "platform::linux::assets::tests::repair_requires_a_receipt_and_non_files_never_gain_implicit_ownership",
            "linux_filesystem::tests::repair_roll_forward_replaces_unknown_binaries_and_changed_or_missing_units",
        }
        self.assertEqual(len(filters), 46)
        self.assertEqual(len(set(filters)), 46)
        self.assertEqual(set(filters), expected_filters)

    def test_macos_proof_authenticates_release_checksums_before_use(self) -> None:
        authenticate = MACOS_WORKFLOW.split(
            "- name: Download and authenticate signed release inputs", 1
        )[1].split("- name: Run the destructive proof", 1)[0]
        self.assertIn("SHA256SUMS.sigstore.json", authenticate)
        self.assertIn(
            'cosign verify-blob --bundle "$dir/SHA256SUMS.sigstore.json"',
            authenticate,
        )
        self.assertLess(
            authenticate.index('"$dir/SHA256SUMS" >/dev/null'),
            authenticate.index('for asset in "pkg-$version-preview.pkg"'),
        )
        for asset in (
            '"pkg-$version-preview.pkg"',
            "pkg-aarch64-darwin",
            "release-manifest.json",
        ):
            self.assertIn(asset, authenticate)
        self.assertIn('manifest.get("releaseId") != sys.argv[2]', authenticate)


class LinuxCleanHostRefusalTests(unittest.TestCase):
    """The real harness must refuse bad input before any Docker or git work."""

    HARNESS = LINUX_RUN_SH

    def _sentinel_path(self, root: Path) -> tuple[Path, Path]:
        """Build a PATH whose docker/git wrappers log one line then fail."""
        sentinels = root / "sentinels"
        sentinels.mkdir()
        log = root / "sentinel.log"
        for name in ("docker", "git"):
            sentinel = sentinels / name
            sentinel.write_text(
                "#!/bin/sh\n"
                f"printf '%s\\n' \"{name} $*\" >> {shlex.quote(str(log))}\n"
                "exit 97\n",
                encoding="utf-8",
            )
            sentinel.chmod(0o700)
        return sentinels, log

    def _run_harness(
        self, argv: list[str], sentinels: Path, cwd: Path
    ) -> subprocess.CompletedProcess[str]:
        try:
            return subprocess.run(
                ["/bin/sh", str(self.HARNESS), *argv],
                capture_output=True,
                text=True,
                timeout=30,
                cwd=str(cwd),
                env={**os.environ, "PATH": str(sentinels)},
            )
        except subprocess.TimeoutExpired as error:
            self.fail(f"clean-host harness did not fail fast: {error}")

    def test_invalid_arguments_refuse_with_exact_usage_and_no_side_effects(self) -> None:
        for argv in (
            ["--keep-artifacts"],
            ["--keep-artifacts", "out", "extra"],
            ["--bogus"],
        ):
            with self.subTest(argv=argv):
                with tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    sentinels, log = self._sentinel_path(root)
                    result = self._run_harness(argv, sentinels, root)
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(
                        result.stderr,
                        f"usage: {self.HARNESS} [--keep-artifacts DIR]\n",
                    )
                    self.assertEqual(result.stdout, "")
                    self.assertFalse(log.exists())
                    self.assertEqual(
                        sorted(path.name for path in root.iterdir()), ["sentinels"]
                    )

    def test_valid_arguments_stop_at_first_docker_attempt_without_git_or_artifacts(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sentinels, log = self._sentinel_path(root)
            artifacts = root / "artifacts"
            result = self._run_harness(
                ["--keep-artifacts", str(artifacts)], sentinels, root
            )
            self.assertEqual(result.returncode, 97)
            self.assertEqual(
                log.read_text(encoding="utf-8").splitlines(),
                ["docker version --format {{.Server.Arch}}"],
            )
            self.assertFalse(artifacts.exists())
            self.assertEqual(
                sorted(path.name for path in root.iterdir()),
                ["sentinel.log", "sentinels"],
            )


if __name__ == "__main__":
    unittest.main()
