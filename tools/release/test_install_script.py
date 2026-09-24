"""Exercise bootstrap trust and status boundaries without root or network access."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InstallScriptTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.work = Path(self.directory.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.payload = self.work / "payload"
        self.payload.write_text('#!/bin/sh\necho "setup reached"; exit "${PKG_TEST_STATUS:-0}"\n')
        self.wrapper = self.work / "wrapper"
        self.wrapper.write_text('#!/bin/sh\ntouch "$PKG_TEST_SUDO"; echo "wrapper reached"; exit "${PKG_TEST_STATUS:-0}"\n')
        self.marker = self.work / "sudo-called"
        for name, code in {
            "uname": 'case "$1" in -s) echo "$PKG_TEST_OS";; -m) echo "$PKG_TEST_ARCH";; esac',
            "curl": 'test "${PKG_TEST_NETWORK:-ok}" = ok || exit 7; while [ "$1" != --output ]; do shift; done; case "$3" in */install-preview.sh) cp "$PKG_TEST_WRAPPER" "$2";; *) cp "$PKG_TEST_PAYLOAD" "$2";; esac',
            "sudo": 'touch "$PKG_TEST_SUDO"; exec "$@"',
            "systemctl": 'exit 0',
            "id": 'echo 501',
            "pkg": 'case "$*" in --version) echo "pkg 0.1.0-alpha.49";; *) echo "health check reached"; exit "${PKG_TEST_HEALTH:-0}";; esac',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\nset -eu\n" + code + "\n")
            path.chmod(0o700)
        source = (ROOT / "docs/install.sh").read_text()
        for key, value in {
            "PKG_RELEASE": "v0.1.0-alpha.49",
            "PKG_ARTIFACT_X86_64_LINUX": "pkg-install-x86_64-linux",
            "PKG_RELEASE_BASE_URL": "https://example.test/releases",
            "PKG_SHA256_X86_64_LINUX": hashlib.sha256(self.payload.read_bytes()).hexdigest(),
            "PKG_SHA256_MACOS_PACKAGE": hashlib.sha256(self.payload.read_bytes()).hexdigest(),
            "PKG_SHA256_MACOS_WRAPPER": hashlib.sha256(self.wrapper.read_bytes()).hexdigest(),
        }.items():
            source = source.replace(f"@{key}@", value)
        # Substitute only host boundaries. Download validation remains the real script.
        source = source.replace('[ -d /run/systemd/system ]', '[ -d "$PKG_TEST_SYSTEMD" ]')
        source = source.replace('pkg_cli=/usr/local/bin/pkg', 'pkg_cli="$PKG_TEST_CLI"')
        self.script = self.work / "install.sh"
        self.script.write_text(source)
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", TMPDIR=str(self.work),
                        PKG_TEST_PAYLOAD=str(self.payload), PKG_TEST_WRAPPER=str(self.wrapper),
                        PKG_TEST_SUDO=str(self.marker), PKG_TEST_SYSTEMD=str(self.work),
                        PKG_TEST_CLI=str(self.bin / 'pkg'), PKG_TEST_OS="Linux", PKG_TEST_ARCH="x86_64")

    def run_script(self, *args, **env):
        return subprocess.run(["sh", str(self.script), *args], env=self.env | env,
                              capture_output=True, text=True)

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
        for path in [self.payload, self.wrapper]:
            original = path.read_bytes()
            path.write_text("changed download")
            result = self.run_script(PKG_TEST_OS="Darwin", PKG_TEST_ARCH="arm64")
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


if __name__ == "__main__":
    unittest.main()
