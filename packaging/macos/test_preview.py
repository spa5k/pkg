"""Run with python3 -m unittest discover -s packaging/macos -p 'test_*.py'."""

import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent


def fixture_binary(path, status):
    # A copied Apple system binary can retain launch constraints that prevent
    # execution after relocation. Compile our own inert Mach-O fixture instead.
    source = path.with_suffix(".c")
    source.write_text(f"int main(void) {{ return {status}; }}\n")
    subprocess.run(["/usr/bin/cc", str(source), "-o", str(path)],
                   check=True, capture_output=True)
    subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(path)],
                   check=True, capture_output=True)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS packaging tools")
class PreviewTests(unittest.TestCase):
    def test_foreground_runner_verifies_package_and_preserves_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            # Replace only privilege elevation. Package expansion and signature
            # verification remain real; neither test executable mutates the host.
            sudo = work / "sudo"
            sudo.write_text('#!/bin/sh\n[ "$1" = -v ] && exit 0\nexec "$@"\n')
            sudo.chmod(0o700)
            runner = work / "install-preview.sh"
            runner.write_text(
                (ROOT / "install-preview.sh").read_text().replace(
                    "/usr/bin/sudo", f'"{sudo}"'
                )
            )
            for name, status in [("true", 0), ("false", 1)]:
                with self.subTest(name=name):
                    package = work / f"{name}.pkg"
                    payload = work / name
                    fixture_binary(payload, status)
                    subprocess.run(
                        ["/bin/sh", str(ROOT / "build-preview.sh"),
                         str(payload), str(package), "0.0.0-test"],
                        check=True, capture_output=True,
                    )
                    digest = hashlib.sha256(package.read_bytes()).hexdigest()
                    result = subprocess.run(
                        ["/bin/bash", str(runner), str(package), digest],
                        env={"TMPDIR": str(work)}, capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, status, result.stderr)
                    self.assertEqual("pkg setup completed." in result.stdout, status == 0)
                    logs = list(work.glob("pkg-install-log.*"))
                    self.assertTrue(logs)
                    self.assertTrue(all(p.stat().st_mode & 0o777 == 0o600 for p in logs))
                    rejected = subprocess.run(
                        ["/bin/bash", str(runner), str(package), "0" * 64],
                        env={"TMPDIR": str(work)}, capture_output=True, text=True,
                    )
                    self.assertNotEqual(rejected.returncode, 0)
                    self.assertIn("checksum mismatch", rejected.stderr)
                    self.assertEqual(len(list(work.glob("pkg-install-log.*"))), len(logs))

    def test_postinstall_returns_real_setup_status(self):
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory)
            postinstall = scripts / "postinstall"
            shutil.copy2(ROOT / "postinstall", postinstall)
            for name, status in [("true", 0), ("false", 1)]:
                with self.subTest(name=name):
                    (scripts / "pkg-install").unlink(missing_ok=True)
                    fixture_binary(scripts / "pkg-install", status)
                    result = subprocess.run(
                        ["/bin/sh", str(postinstall)], capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, status, result.stderr)
                    self.assertEqual("pkg setup failed" in result.stderr, status != 0)


if __name__ == "__main__":
    unittest.main()
