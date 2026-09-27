"""Run with python3 -m unittest discover -s packaging/macos -p 'test_*.py'."""

import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent


def fixture_binary(path, status, output=""):
    # A copied Apple system binary can retain launch constraints that prevent
    # execution after relocation. Compile our own inert Mach-O fixture instead.
    source = path.with_suffix(".c")
    source.write_text(
        f"#include <stdio.h>\nint main(void) {{ puts({json.dumps(output)}); return {status}; }}\n"
    )
    subprocess.run(["/usr/bin/cc", str(source), "-o", str(path)],
                   check=True, capture_output=True)
    subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(path)],
                   check=True, capture_output=True)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS packaging tools")
class PreviewTests(unittest.TestCase):
    def prepare_runner(self, work, cli, approval_required=False):
        # Replace privilege elevation and the installed path only. Package
        # expansion, signatures, file permissions, and child execution are real.
        sudo = work / "sudo"
        sudo.write_text(
            '#!/bin/sh\n'
            '[ "$1" != -n ] || shift\n'
            f'[ "$1" != -v ] || exit {1 if approval_required else 0}\n'
            'exec "$@"\n'
        )
        sudo.chmod(0o700)
        runner = work / "install-preview.sh"
        runner.write_text(
            (ROOT / "install-preview.sh").read_text()
            .replace("/usr/bin/sudo", shlex.quote(str(sudo)))
            .replace("pkg_cli=/usr/local/bin/pkg", "pkg_cli=" + shlex.quote(str(cli)))
        )
        return runner

    def test_foreground_runner_verifies_package_and_preserves_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            cli = work / "pkg"
            fixture_binary(cli, 0, "pkg 0.0.0-test")
            runner = self.prepare_runner(work, cli)
            for name, status in [("true", 0), ("false", 37)]:
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

    def test_success_requires_cli_access_and_execution_as_the_invoking_user(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            cli_parent = work / "bin"
            cli_parent.mkdir()
            cli = cli_parent / "pkg"
            runner = self.prepare_runner(work, cli)
            payload = work / "pkg-install"
            fixture_binary(payload, 0, "INSTALLER_RAN")
            package = work / "success.pkg"
            subprocess.run(
                ["/bin/sh", str(ROOT / "build-preview.sh"), str(payload),
                 str(package), "0.0.0-test"], check=True, capture_output=True,
            )
            digest = hashlib.sha256(package.read_bytes()).hexdigest()
            for state in ["missing", "not-executable", "blocked-parent", "launch-failure", "ready"]:
                with self.subTest(state=state):
                    cli.unlink(missing_ok=True)
                    if state != "missing":
                        fixture_binary(cli, 73 if state == "launch-failure" else 0,
                                       "pkg 0.0.0-test")
                    if state == "not-executable":
                        cli.chmod(0o644)
                    if state == "blocked-parent":
                        cli_parent.chmod(0o600)
                    previous_logs = set(work.glob("pkg-install-log.*"))
                    try:
                        result = subprocess.run(
                            ["/bin/bash", str(runner), str(package), digest],
                            env={"TMPDIR": str(work)}, capture_output=True, text=True,
                        )
                    finally:
                        cli_parent.chmod(0o700)
                    ready = state == "ready"
                    self.assertEqual(result.returncode, 0 if ready else 1, result.stdout + result.stderr)
                    self.assertEqual("pkg setup completed." in result.stdout, ready)
                    self.assertEqual("Next steps:" in result.stdout, ready)
                    if not ready:
                        expected_error = ("Your user cannot enter" if state == "blocked-parent"
                                          else "pkg command check failed")
                        self.assertIn(expected_error, result.stderr)
                        logs = set(work.glob("pkg-install-log.*")) - previous_logs
                        self.assertEqual(len(logs), 1)
                        log = logs.pop()
                        self.assertIn(expected_error, log.read_text())
                        self.assertIn("State before setup", log.read_text())
                        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
                    self.assertEqual("INSTALLER_RAN" in result.stdout, state != "blocked-parent")

    def test_command_directory_state_controls_installer_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            payload = work / "pkg-install"
            fixture_binary(payload, 0, "INSTALLER_RAN")
            package = work / "success.pkg"
            subprocess.run(
                ["/bin/sh", str(ROOT / "build-preview.sh"), str(payload),
                 str(package), "0.0.0-test"], check=True, capture_output=True,
            )
            digest = hashlib.sha256(package.read_bytes()).hexdigest()
            for state in ["missing-prefix", "missing-bin", "symlink", "file"]:
                with self.subTest(state=state):
                    prefix = work / state
                    parent = prefix / "bin"
                    if state != "missing-prefix":
                        prefix.mkdir()
                    if state == "symlink":
                        parent.symlink_to(work, target_is_directory=True)
                    elif state == "file":
                        parent.write_text("unrelated file")
                    runner = self.prepare_runner(work, parent / "pkg")
                    result = subprocess.run(
                        ["/bin/bash", str(runner), str(package), digest],
                        env={"TMPDIR": str(work)}, capture_output=True, text=True,
                    )
                    # Missing parents reach the installer. This inert fixture
                    # installs nothing, so the final command check must fail.
                    missing = state.startswith("missing-")
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertEqual("INSTALLER_RAN" in result.stdout, missing)
                    self.assertNotIn("pkg setup completed.", result.stdout)
                    if missing:
                        self.assertIn("pkg command check failed", result.stderr)
                        self.assertFalse(parent.exists())
                    elif state == "symlink":
                        self.assertIn("symbolic link", result.stderr)
                        self.assertTrue(parent.is_symlink())
                    else:
                        self.assertIn("not a directory", result.stderr)
                        self.assertEqual(parent.read_text(), "unrelated file")

    def test_password_required_without_a_terminal_fails_before_installation(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            cli = work / "pkg"
            runner = self.prepare_runner(work, cli, approval_required=True)
            payload = work / "pkg-install"
            fixture_binary(payload, 0, "INSTALLER_RAN")
            package = work / "success.pkg"
            subprocess.run(
                ["/bin/sh", str(ROOT / "build-preview.sh"), str(payload),
                 str(package), "0.0.0-test"], check=True, capture_output=True,
            )
            result = subprocess.run(
                ["/bin/bash", str(runner), str(package), hashlib.sha256(package.read_bytes()).hexdigest()],
                env={"TMPDIR": str(work)}, capture_output=True, text=True,
                start_new_session=True,
            )
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("Open Terminal", result.stderr)
            self.assertNotIn("INSTALLER_RAN", result.stdout)
            self.assertNotIn("pkg setup completed.", result.stdout)
            logs = list(work.glob("pkg-install-log.*"))
            self.assertEqual(len(logs), 1)
            self.assertIn("Open Terminal", logs[0].read_text())
            self.assertIn("State before setup", logs[0].read_text())
            self.assertEqual(logs[0].stat().st_mode & 0o777, 0o600)

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
