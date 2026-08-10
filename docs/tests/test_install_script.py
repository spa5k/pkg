#!/usr/bin/env python3
"""Security-focused checks for the unpublished install-script template."""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "docs" / "install.sh"


class InstallScriptTests(unittest.TestCase):
    def test_unrendered_template_refuses_before_network(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "curl-was-called"
            fake_curl = pathlib.Path(directory) / "curl"
            fake_curl.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 99\n")
            fake_curl.chmod(0o700)
            result = subprocess.run(
                ["sh", str(SCRIPT)],
                env={"PATH": f"{directory}:{os.environ['PATH']}"},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("unpublished release", result.stderr)
            self.assertFalse(marker.exists())

    def test_template_has_closed_https_and_checksum_contract(self) -> None:
        text = SCRIPT.read_text()
        self.assertIn("--proto '=https'", text)
        self.assertIn("--proto-redir '=https'", text)
        self.assertIn("sha256sum --check", text)
        self.assertIn("shasum -a 256 --check", text)
        self.assertNotIn("curl |", text)
        self.assertNotIn("eval ", text)
        self.assertNotIn("PKG_URL=", text)
        self.assertNotIn("PKG_SHA256=", text)

    def test_only_verify_only_is_accepted(self) -> None:
        result = subprocess.run(
            ["sh", str(SCRIPT), "--url", "https://attacker.invalid"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
