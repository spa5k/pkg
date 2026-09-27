"""Exercise bootstrap trust and status boundaries without root or network access.

The script under test is the real render.py CLI output: release tag, base URL,
artifact name, and the three SHA-256 digests all come from rendering a complete
distinct fixture at the subprocess boundary. Only host boundaries (systemd
detection and the installed CLI path) are substituted after rendering, so the
download, digest, privilege, and health logic stays the real script.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TAG = "v0.1.0-alpha.49"

LINUX_ARTIFACT_TEXT = '#!/bin/sh\necho "linux artifact setup reached"; exit "${PKG_TEST_STATUS:-0}"\n'
MACOS_PACKAGE_BYTES = b"pkg macOS preview package fixture bytes for 0.1.0-alpha.49 (never executed here)\n"
WRAPPER_TEXT = '#!/bin/sh\ntouch "$PKG_TEST_SUDO"; echo "wrapper reached"; exit "${PKG_TEST_STATUS:-0}"\n'


class InstallScriptTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.work = Path(self.directory.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.assets = self.work / "release-assets"
        self.assets.mkdir()
        self.linux_artifact = self.assets / "pkg-install-x86_64-linux"
        self.linux_artifact.write_text(LINUX_ARTIFACT_TEXT)
        self.macos_package = self.assets / f"pkg-{TAG[1:]}-preview.pkg"
        self.macos_package.write_bytes(MACOS_PACKAGE_BYTES)
        self.wrapper = self.assets / "install-preview.sh"
        self.wrapper.write_text(WRAPPER_TEXT)
        self.marker = self.work / "sudo-called"
        for name, code in {
            "uname": 'case "$1" in -s) echo "$PKG_TEST_OS";; -m) echo "$PKG_TEST_ARCH";; esac',
            "curl": 'test "${PKG_TEST_NETWORK:-ok}" = ok || exit 7; while [ "$1" != --output ]; do shift; done; case "$3" in */pkg-install-x86_64-linux) cp "$PKG_TEST_LINUX_ARTIFACT" "$2";; *-preview.pkg) cp "$PKG_TEST_MACOS_PACKAGE" "$2";; */install-preview.sh) cp "$PKG_TEST_WRAPPER" "$2";; *) echo "unexpected download: $3" >&2; exit 7;; esac',
            "sudo": 'touch "$PKG_TEST_SUDO"; exec "$@"',
            "systemctl": 'exit 0',
            "id": 'echo 501',
            "pkg": 'case "$*" in --version) echo "pkg 0.1.0-alpha.49";; *) echo "health check reached"; exit "${PKG_TEST_HEALTH:-0}";; esac',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\nset -eu\n" + code + "\n")
            path.chmod(0o700)
        rendered = subprocess.run(
            [sys.executable, str(ROOT / "tools/install/render.py"), "--", TAG, str(self.assets), str(self.work / "rendered-install.sh")],
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        source = (self.work / "rendered-install.sh").read_text()
        # Real shasum verification checks each rendered digest against downloaded
        # bytes. Distinct payloads also detect hashes assigned to the wrong asset.
        # Substitute only host boundaries. Download validation remains the real script.
        source = source.replace('[ -d /run/systemd/system ]', '[ -d "$PKG_TEST_SYSTEMD" ]')
        source = source.replace('pkg_cli=/usr/local/bin/pkg', 'pkg_cli="$PKG_TEST_CLI"')
        self.script = self.work / "install.sh"
        self.script.write_text(source)
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", TMPDIR=str(self.work),
                        PKG_TEST_LINUX_ARTIFACT=str(self.linux_artifact), PKG_TEST_MACOS_PACKAGE=str(self.macos_package),
                        PKG_TEST_WRAPPER=str(self.wrapper),
                        PKG_TEST_SUDO=str(self.marker), PKG_TEST_SYSTEMD=str(self.work),
                        PKG_TEST_CLI=str(self.bin / 'pkg'), PKG_TEST_OS="Linux", PKG_TEST_ARCH="x86_64")

    def run_script(self, *args, **env):
        return subprocess.run(["sh", str(self.script), *args], env=self.env | env,
                              capture_output=True, text=True, timeout=10)

    def test_both_platforms_verify_without_elevation(self):
        for system, arch in [("Linux", "x86_64"), ("Darwin", "arm64")]:
            with self.subTest(system=system):
                result = self.run_script("--verify-only", PKG_TEST_OS=system, PKG_TEST_ARCH=arch)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Downloads verified. No changes", result.stdout)
                self.assertFalse(self.marker.exists())
                self.assertNotIn("\x1b", result.stdout)

    def test_both_platforms_keep_real_failure_status_and_private_log(self):
        for system, arch in [("Linux", "x86_64"), ("Darwin", "arm64")]:
            with self.subTest(system=system):
                result = self.run_script(PKG_TEST_OS=system, PKG_TEST_ARCH=arch, PKG_TEST_STATUS="17")
                self.assertEqual(result.returncode, 17, result.stderr)
                self.assertIn("Keep this log", result.stderr)
                self.assertNotIn("pkg is ready", result.stdout)
                self.assertTrue(self.marker.exists())
        logs = list(self.work.glob("pkg-install-log.*"))
        self.assertEqual(len(logs), 2)
        self.assertTrue(all(p.stat().st_mode & 0o777 == 0o600 for p in logs))
        self.assertFalse(list(self.work.glob("pkg-install.*")))

    def test_tampered_package_and_wrapper_never_execute(self):
        cases = [(self.macos_package, "Darwin", "arm64"), (self.wrapper, "Darwin", "arm64"),
                 (self.linux_artifact, "Linux", "x86_64")]
        for path, system, arch in cases:
            with self.subTest(artifact=path.name, system=system):
                original = path.read_bytes()
                path.write_bytes(original + b"\nchanged download\n")
                result = self.run_script(PKG_TEST_OS=system, PKG_TEST_ARCH=arch)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Checksum mismatch", result.stderr)
                self.assertFalse(self.marker.exists())
                path.write_bytes(original)

    def test_setup_success_requires_healthy_installed_version(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("health check reached", result.stdout)
        self.assertIn("pkg is ready", result.stdout)
        failed = self.run_script(PKG_TEST_HEALTH="78")
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("health check failed", failed.stderr)
        self.assertNotIn("pkg is ready", failed.stdout)

    def test_unsupported_host_and_network_failure_never_execute(self):
        for env in [{"PKG_TEST_ARCH": "riscv64"}, {"PKG_TEST_NETWORK": "failed"}]:
            result = self.run_script(**env)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.marker.exists())

    def test_doctor_receives_idempotent_path_through_the_complete_bootstrap(self):
        observed = self.work / "doctor-path"
        home = self.work / "home with spaces"
        (self.bin / "pkg").write_text(
            '#!/bin/sh\n'
            'if [ "$1" = --version ]; then echo "pkg 0.1.0-alpha.49"; exit 0; fi\n'
            'printf "%s" "$PATH" > "$PKG_TEST_DOCTOR_PATH"\n'
        )
        base = self.env["PATH"]
        for system, arch, suffix in [
            ("Darwin", "arm64", "Library/Application Support/pkg/current/bin"),
            ("Linux", "x86_64", ".local/share/pkg/current/bin"),
        ]:
            managed = str(home / suffix)
            for initial in [base, f"{managed}:/usr/local/bin:{base}", f"/usr/local/bin:{base}"]:
                with self.subTest(system=system, initial=initial):
                    previous = None
                    for _ in range(2):
                        observed.unlink(missing_ok=True)
                        result = self.run_script(
                            PATH=initial, HOME=str(home), PKG_TEST_OS=system,
                            PKG_TEST_ARCH=arch, PKG_TEST_DOCTOR_PATH=str(observed),
                        )
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertTrue(observed.is_file(), "doctor did not capture its PATH")
                        actual = observed.read_text()
                        self.assertEqual(actual.split(":").count(managed), 1)
                        self.assertEqual(actual.split(":").count("/usr/local/bin"), 1)
                        if previous is not None:
                            self.assertEqual(actual, previous)
                        previous = initial = actual

    def test_saved_log_retains_failures_after_the_installer_returns(self):
        cli = self.bin / "pkg"
        original = cli.read_bytes()
        cases = [
            ("missing", "missing or cannot be executed", None, None),
            ("launch", "could not start", '#!/bin/sh\necho "launch denied" >&2; exit 73\n', None),
            ("version", "not the requested release", '#!/bin/sh\necho "pkg wrong-version"\n', None),
            ("health", "health check failed", None, "78"),
        ]
        for name, diagnostic, source, health in cases:
            with self.subTest(case=name):
                cli.write_bytes(original)
                cli.chmod(0o700)
                if name == "missing":
                    cli.unlink()
                elif source:
                    cli.write_text(source)
                previous = set(self.work.glob("pkg-install-log.*"))
                result = self.run_script(**({"PKG_TEST_HEALTH": health} if health else {}))
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(diagnostic, result.stderr)
                self.assertNotIn("pkg is ready", result.stdout)
                logs = set(self.work.glob("pkg-install-log.*")) - previous
                self.assertEqual(len(logs), 1)
                log = logs.pop()
                self.assertIn(diagnostic, log.read_text())
                self.assertEqual(log.stat().st_mode & 0o777, 0o600)
                if name == "launch":
                    self.assertIn("launch denied", log.read_text())
                if name == "health":
                    self.assertIn("health check reached", log.read_text())

    def test_health_status_write_failure_cannot_report_ready(self):
        # Cause a real filesystem write failure at the process-status boundary.
        # tee can still succeed, so its exit status cannot prove doctor passed.
        (self.bin / "pkg").write_text(
            '#!/bin/sh\n'
            'if [ "$1" = --version ]; then echo "pkg 0.1.0-alpha.49"; exit 0; fi\n'
            'for setup in "$TMPDIR"/pkg-install.*; do mkdir "$setup/health-status"; done\n'
            'echo "health check failed before status could be saved"; exit 78\n'
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("pkg is ready", result.stdout)
        self.assertIn("Could not read the health check result", result.stderr)
        logs = list(self.work.glob("pkg-install-log.*"))
        self.assertEqual(len(logs), 1)
        self.assertIn("Could not read the health check result", logs[0].read_text())


if __name__ == "__main__":
    unittest.main()
