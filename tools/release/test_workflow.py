"""Structural release-workflow security contract."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
PUBLISH_WORKFLOW = (ROOT / ".github/workflows/publish-release.yml").read_text(
    encoding="utf-8"
)
LINUX_HARNESS = (ROOT / "tests/linux-clean-host/run.sh").read_text(encoding="utf-8")
LINUX_STAGE = (ROOT / "tests/linux-clean-host/Dockerfile.stage").read_text(
    encoding="utf-8"
)
LINUX_HOST = (ROOT / "tests/linux-clean-host/Dockerfile").read_text(encoding="utf-8")
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
        self.assertIn("flag=--json", LINUX_HARNESS)
        self.assertIn("flag=--jsonl", LINUX_HARNESS)
        self.assertEqual(
            LINUX_HARNESS.count(
                'docker exec "$container" /usr/local/bin/pkg "$flag" --yes uninstall'
            ),
            1,
        )
        self.assertIn('test "$status" -eq 78', LINUX_HARNESS)
        self.assertIn('test ! -s "$stderr"', LINUX_HARNESS)
        self.assertIn('cmp "$before" "$after"', LINUX_HARNESS)
        self.assertNotIn("pkg-after-uninstall", LINUX_HARNESS)
        self.assertNotIn("idempotent uninstall", LINUX_HARNESS)

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
            "terminal-uninstall",
        ):
            self.assertIn(case, LINUX_HARNESS)
        self.assertIn('"$results")" -eq 34', LINUX_HARNESS)
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
        self.assertIn("/binaries-n x86_64-linux /proof-signing 1", LINUX_STAGE)
        self.assertIn(
            "/binaries-n-plus-1 x86_64-linux /proof-signing 2", LINUX_STAGE
        )
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
        for boundary in (
            "package-state-after-upgrade.txt",
            "package-state-after-active-repair-refusal.txt",
            "package-state-after-repair.txt",
        ):
            self.assertIn(boundary, LINUX_HARNESS)
            self.assertIn(
                'cmp "$product_evidence/package-state-before.txt" \\\n'
                f'    "$product_evidence/{boundary}"',
                LINUX_HARNESS,
            )
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
            "uninstall::tests::linux_product_cleanup_failure_never_dispatches_terminal_vendor",
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
            "linux_platform_assets::tests::ordinary_upgrade_requires_different_release_and_prior_content_identity",
            "linux_filesystem::tests::upgrade_replaces_only_exact_prior_owned_bytes_and_rolls_back",
            "bootstrap::tests::journaled_offline_repair_changes_product_files_without_service_mutation",
            "bootstrap::tests::journaled_repair_refuses_non_offline_service_state_before_mutation",
            "bootstrap::tests::failed_offline_repair_rolls_forward_files_without_service_mutation",
            "linux_systemd::tests::offline_preflight_is_query_only_and_refuses_every_non_offline_state",
            "linux_platform_assets::tests::repair_requires_same_release_and_created_product_ownership",
            "linux_platform_assets::tests::repair_requires_a_receipt_and_non_files_never_gain_implicit_ownership",
            "linux_filesystem::tests::repair_roll_forward_replaces_unknown_binaries_and_changed_or_missing_units",
        }
        self.assertEqual(len(filters), 46)
        self.assertEqual(len(set(filters)), 46)
        self.assertEqual(set(filters), expected_filters)

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
