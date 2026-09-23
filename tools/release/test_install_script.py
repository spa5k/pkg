"""Check the bootstrap without network access or administrator privileges."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InstallScriptTests(unittest.TestCase):
    def test_verification_and_real_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            binary = work / "download"
            binary.write_text("#!/bin/sh\necho 'setup reached'; exit 17\n")
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            stubs = work / "bin"
            stubs.mkdir()
            for name, code in {
                "uname": 'case "$1" in -s) echo Linux;; -m) echo x86_64;; esac',
                "curl": 'while [ "$1" != --output ]; do shift; done; cp "$PKG_TEST_PAYLOAD" "$2"',
                "sudo": 'touch "$PKG_TEST_SUDO"; exec "$@"',
            }.items():
                path = stubs / name
                path.write_text("#!/bin/sh\nset -eu\n" + code + "\n")
                path.chmod(0o700)
            script = work / "install.sh"
            source = (ROOT / "docs/install.sh").read_text()
            source = source.replace("@PKG_RELEASE@", "v0.1.0-alpha.46")
            source = source.replace("@PKG_RELEASE_BASE_URL@", "https://example.test/releases")
            source = source.replace("@PKG_SHA256_X86_64_LINUX@", digest)
            script.write_text(source)
            marker = work / "sudo-called"
            env = dict(os.environ, PATH=f"{stubs}:/usr/bin:/bin", TMPDIR=directory,
                       PKG_TEST_PAYLOAD=str(binary), PKG_TEST_SUDO=str(marker))
            verified = subprocess.run(["sh", str(script), "--verify-only"], env=env,
                                      capture_output=True, text=True)
            self.assertEqual(verified.returncode, 0, verified.stderr)
            self.assertFalse(marker.exists())
            failed = subprocess.run(["sh", str(script)], env=env, capture_output=True, text=True)
            self.assertEqual(failed.returncode, 17, failed.stderr)
            self.assertIn("setup reached", failed.stdout)
            self.assertIn("Keep this log", failed.stderr)
            logs = list(work.glob("pkg-install-log.*"))
            self.assertEqual(len(logs), 1)
            self.assertEqual(logs[0].stat().st_mode & 0o777, 0o600)
            self.assertIn("setup reached", logs[0].read_text())
            marker.unlink()
            binary.write_text("changed download")
            refused = subprocess.run(["sh", str(script)], env=env, capture_output=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
