#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import pathlib
import re
import stat
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("stage_linux_alpha.py")
SPEC = importlib.util.spec_from_file_location("stage_linux_alpha", SCRIPT)
assert SPEC and SPEC.loader
STAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGER)
ROOT = pathlib.Path(__file__).resolve().parents[2]


def elf(machine: int) -> bytes:
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header)


class StageLinuxAlphaTests(unittest.TestCase):
    def test_stages_exact_versioned_artifact_and_bootstrap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "pkg-install"
            binary.write_bytes(elf(62))
            output = root / "output"
            STAGER.stage(
                binary,
                ROOT / "docs/install.sh",
                output,
                "https://releases.pkg.example/alpha",
            )
            artifact = output / STAGER.RELEASE / STAGER.ARTIFACT
            bootstrap = output / "install.sh"
            self.assertEqual(artifact.read_bytes(), elf(62))
            self.assertEqual(stat.S_IMODE(artifact.stat().st_mode), 0o755)
            self.assertIsNone(re.search(r"@PKG_[A-Z0-9_]+@", bootstrap.read_text()))
            self.assertIn(
                "PKG_RELEASE_BASE_URL='https://releases.pkg.example/alpha'",
                bootstrap.read_text(),
            )
            self.assertIn(f"PKG_RELEASE='{STAGER.RELEASE}'", bootstrap.read_text())
            self.assertIn(
                'pkg_url="$PKG_RELEASE_BASE_URL/$PKG_RELEASE/$pkg_artifact"',
                bootstrap.read_text(),
            )
            self.assertEqual(stat.S_IMODE(bootstrap.stat().st_mode), 0o755)
            self.assertEqual(len((output / "SHA256SUMS").read_text().splitlines()), 2)

    def test_refuses_foreign_architecture_before_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "pkg-install"
            binary.write_bytes(elf(183))
            output = root / "output"
            with self.assertRaisesRegex(ValueError, "x86-64 ELF"):
                STAGER.stage(
                    binary,
                    ROOT / "docs/install.sh",
                    output,
                    "https://releases.pkg.example/alpha",
                )
            self.assertFalse(output.exists())

    def test_refuses_mutable_or_insecure_release_locations(self) -> None:
        for value in (
            "http://releases.pkg.example",
            "https://user@releases.pkg.example",
            "https://releases.pkg.example/path?channel=latest",
        ):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    STAGER.require_https_base_url(value)


if __name__ == "__main__":
    unittest.main(verbosity=2)
