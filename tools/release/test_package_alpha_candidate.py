#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import pathlib
import re
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("package_alpha_candidate.py")
SPEC = importlib.util.spec_from_file_location("package_alpha_candidate", SCRIPT)
assert SPEC and SPEC.loader
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)
ROOT = pathlib.Path(__file__).resolve().parents[2]


def elf(machine: int = 62) -> bytes:
    image = bytearray(PACKAGER.MIN_INSTALLER_SIZE)
    image[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<HHI", image, 16, 3, machine, 1)
    struct.pack_into("<QQ", image, 24, 0x1000, 64)
    struct.pack_into("<HHH", image, 52, 64, 56, 1)
    struct.pack_into(
        "<IIQQQQQQ",
        image,
        64,
        1,
        5,
        0,
        0x1000,
        0x1000,
        len(image),
        len(image),
        0x1000,
    )
    return bytes(image)


def macho(cpu: int = 0x0100000C) -> bytes:
    image = bytearray(PACKAGER.MIN_INSTALLER_SIZE)
    struct.pack_into("<IIIIIIII", image, 0, 0xFEEDFACF, cpu, 0, 2, 2, 96, 0, 0)
    struct.pack_into(
        "<II16sQQQQIIII",
        image,
        32,
        0x19,
        72,
        b"__TEXT",
        0x1000,
        len(image),
        0,
        len(image),
        7,
        5,
        0,
        0,
    )
    struct.pack_into("<IIQQ", image, 104, 0x80000028, 24, 0, 0)
    return bytes(image)


def xar() -> bytes:
    return b"xar!" + bytes(PACKAGER.MIN_PACKAGE_SIZE - 4)


def nix_source(path: pathlib.Path) -> str:
    copying = b"GNU LESSER GENERAL PUBLIC LICENSE\nVersion 2.1\n"
    with tarfile.open(path, "w:gz") as archive:
        info = tarfile.TarInfo(PACKAGER.NIX_SOURCE_MEMBER)
        info.size = len(copying)
        archive.addfile(info, io.BytesIO(copying))
    return hashlib.sha256(path.read_bytes()).hexdigest()


class PackageAlphaCandidateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.directory.name)
        self.third_party = (
            b'<h1>Third-Party Licenses</h1><li data-crate="serde">serde 1</li><pre>MIT</pre>'
        )
        self.nix_source = self.root / "nix-2.34.8.tar.gz"
        self.nix_sha = nix_source(self.nix_source)

    def tearDown(self) -> None:
        self.directory.cleanup()

    def package(self, platform: str, staged: pathlib.Path, output: pathlib.Path) -> None:
        patches = [
            mock.patch.object(PACKAGER, "NIX_SOURCE_SHA256", self.nix_sha),
            mock.patch.object(
                PACKAGER,
                "generate_third_party",
                return_value=self.third_party,
            ),
        ]
        if platform == "macos-aarch64":
            installer = (
                staged / PACKAGER.RELEASE / PACKAGER.MACOS_INSTALLER
            ).read_bytes()

            def expand(arguments: list[str], **_: object) -> subprocess.CompletedProcess[bytes]:
                if arguments[0] == "/usr/sbin/pkgutil":
                    self.write_expanded(pathlib.Path(arguments[-1]), installer)
                return subprocess.CompletedProcess(
                    arguments,
                    0,
                    stderr="Signature=adhoc\n" if "--display" in arguments else b"",
                )

            patches.extend(
                [
                    mock.patch.object(PACKAGER.sys, "platform", "darwin"),
                    mock.patch.object(PACKAGER.subprocess, "run", side_effect=expand),
                ]
            )
        with contextlib.ExitStack() as stack:
            for patch in patches:
                stack.enter_context(patch)
            PACKAGER.package_candidate(
                platform,
                staged,
                ROOT / "LICENSE",
                self.root / "cargo-about",
                self.nix_source,
                output,
            )

    def write_expanded(self, root: pathlib.Path, installer: bytes) -> None:
        scripts = root / "Scripts"
        scripts.mkdir(parents=True)
        (scripts / "pkg-install").write_bytes(installer)
        (scripts / "postinstall").write_bytes(
            (ROOT / "packaging/macos/postinstall").read_bytes()
        )
        (root / "PackageInfo").write_text(
            '<pkg-info identifier="org.pkg.installer.preview" '
            'version="0.1.0-alpha.7" auth="root">'
            '<scripts><postinstall file="./postinstall"/></scripts></pkg-info>'
        )

    def test_release_matches_workspace_version(self) -> None:
        version = re.search(
            r'^version = "([^"]+)"$',
            (ROOT / "Cargo.toml").read_text(),
            re.MULTILINE,
        )
        self.assertIsNotNone(version)
        self.assertEqual(PACKAGER.RELEASE, f"v{version.group(1)}")

    def test_published_preview_notes_do_not_claim_local_proof(self) -> None:
        notes = PACKAGER.release_notes("linux-x86_64", published_preview=True)
        self.assertIn(b"signed TUF metadata", notes)
        self.assertNotIn(b"NOT FOR PUBLICATION", notes)
        self.assertNotIn(b"loopback", notes)

    def test_linux_archive_has_exact_checked_contents(self) -> None:
        staged = self.root / "linux"
        (staged / PACKAGER.RELEASE).mkdir(parents=True)
        (staged / "install.sh").write_text("#!/bin/sh\n")
        (staged / PACKAGER.RELEASE / PACKAGER.LINUX_ARTIFACT).write_bytes(elf())
        output = self.root / "linux.tar.gz"
        self.package("linux-x86_64", staged, output)

        with tarfile.open(output, "r:gz") as archive:
            members = {member.name: member for member in archive.getmembers() if member.isfile()}
            expected = {
                "LICENSE",
                "NIX-LICENSE",
                "NIX-SOURCE.md",
                "RELEASE_NOTES.md",
                "SHA256SUMS",
                "THIRD_PARTY_LICENSES.html",
                "install.sh",
                f"{PACKAGER.RELEASE}/{PACKAGER.LINUX_ARTIFACT}",
            }
            self.assertEqual(set(members), expected)
            checksums = archive.extractfile(members["SHA256SUMS"]).read().decode()
            covered = {line.split("  ", 1)[1] for line in checksums.splitlines()}
            self.assertEqual(covered, expected - {"SHA256SUMS"})
            notes = archive.extractfile(members["RELEASE_NOTES.md"]).read()
            self.assertIn(b"NOT FOR PUBLICATION", notes)
            self.assertIn(b"loopback", notes)
            self.assertFalse(
                any(
                    name.endswith(("root.json", ".key", ".pk8", ".sigstore.json"))
                    or "publication-" in name
                    for name in members
                )
            )

    def test_macos_archive_refuses_wrong_formats_before_output(self) -> None:
        staged = self.root / "macos"
        (staged / PACKAGER.RELEASE).mkdir(parents=True)
        installer = staged / PACKAGER.RELEASE / PACKAGER.MACOS_INSTALLER
        package = staged / PACKAGER.RELEASE / PACKAGER.MACOS_PACKAGE
        output = self.root / "macos.tar.gz"
        installer.write_bytes(elf())
        package.write_bytes(xar())
        with self.assertRaisesRegex(ValueError, "arm64 Mach-O"):
            self.package("macos-aarch64", staged, output)
        self.assertFalse(output.exists())

        installer.write_bytes(macho())
        package.write_bytes(b"not a package")
        with self.assertRaisesRegex(ValueError, "XAR"):
            self.package("macos-aarch64", staged, output)
        self.assertFalse(output.exists())

    def test_macos_archive_accepts_structural_executables(self) -> None:
        staged = self.root / "macos-valid"
        (staged / PACKAGER.RELEASE).mkdir(parents=True)
        (staged / PACKAGER.RELEASE / PACKAGER.MACOS_INSTALLER).write_bytes(macho())
        (staged / PACKAGER.RELEASE / PACKAGER.MACOS_PACKAGE).write_bytes(xar())
        output = self.root / "macos-valid.tar.gz"
        self.package("macos-aarch64", staged, output)
        self.assertTrue(output.is_file())

    def test_expanded_macos_package_refuses_changed_payloads(self) -> None:
        expanded = self.root / "expanded"
        installer = macho()
        self.write_expanded(expanded, installer)
        PACKAGER.validate_expanded_macos_package(expanded, installer)

        (expanded / "Scripts/pkg-install").write_bytes(installer[:-1] + b"x")
        with self.assertRaisesRegex(ValueError, "different pkg-install"):
            PACKAGER.validate_expanded_macos_package(expanded, installer)

        (expanded / "Scripts/pkg-install").write_bytes(installer)
        (expanded / "Scripts/postinstall").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "different postinstall"):
            PACKAGER.validate_expanded_macos_package(expanded, installer)

    def test_expanded_macos_package_refuses_unexpected_file(self) -> None:
        expanded = self.root / "expanded-extra"
        installer = macho()
        self.write_expanded(expanded, installer)
        (expanded / "unexpected").write_bytes(b"data")
        with self.assertRaisesRegex(ValueError, "unexpected expanded files"):
            PACKAGER.validate_expanded_macos_package(expanded, installer)

    @unittest.skipUnless(PACKAGER.sys.platform == "darwin", "requires macOS")
    def test_real_script_only_package_has_the_expected_expansion(self) -> None:
        source = self.root / "source-installer"
        subprocess.run(
            ["/usr/bin/lipo", "/usr/bin/curl", "-thin", "arm64e", "-output", source],
            check=True,
        )
        source.chmod(0o755)
        package = self.root / "preview.pkg"
        subprocess.run(
            [ROOT / "packaging/macos/build-preview.sh", source, package],
            check=True,
            capture_output=True,
        )
        expanded = self.root / "native-expanded"
        subprocess.run(
            ["/usr/sbin/pkgutil", "--expand-full", package, expanded],
            check=True,
            capture_output=True,
        )
        installer = (expanded / "Scripts/pkg-install").read_bytes()
        with (
            mock.patch.object(PACKAGER, "MIN_INSTALLER_SIZE", 0),
            mock.patch.object(PACKAGER, "MIN_PACKAGE_SIZE", 0),
        ):
            PACKAGER.require_macos_installer(installer)
            PACKAGER.require_macos_package(package.read_bytes(), installer)

    def test_truncated_executable_and_license_report_are_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "x86-64 ELF"):
            PACKAGER.require_linux_installer(b"\x7fELF\x02\x01" + bytes(58))
        with self.assertRaisesRegex(ValueError, "third-party"):
            PACKAGER.require_third_party(b"<h1>Third-Party Licenses</h1>")

    def test_license_generation_is_offline_and_fail_closed(self) -> None:
        calls: list[list[str]] = []
        cargo_about = self.root / "cargo-about"
        cargo_about.write_bytes(b"fake")

        def run(arguments: list[str], **_: object) -> subprocess.CompletedProcess[str]:
            calls.append(arguments)
            if "--version" in arguments:
                return subprocess.CompletedProcess(arguments, 0, stdout="cargo-about 0.9.1\n")
            output = pathlib.Path(arguments[arguments.index("--output-file") + 1])
            output.write_bytes(self.third_party)
            return subprocess.CompletedProcess(arguments, 0, stdout="")

        with (
            mock.patch.object(
                PACKAGER,
                "require_regular",
                side_effect=lambda path, _: pathlib.Path(path).read_bytes(),
            ),
            mock.patch.object(PACKAGER.subprocess, "run", side_effect=run),
        ):
            report = PACKAGER.generate_third_party(cargo_about)
        self.assertEqual(report, self.third_party)
        self.assertIn("--frozen", calls[1])
        self.assertIn("--fail", calls[1])

    def test_source_checksum_and_existing_output_fail_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "checksum"):
            PACKAGER.require_nix_copying(self.nix_source)

        output = self.root / "candidate.tar.gz"
        output.write_bytes(b"keep")
        with self.assertRaisesRegex(ValueError, "already exists"):
            PACKAGER.write_archive({"file": (b"data", 0o644)}, output)
        self.assertEqual(output.read_bytes(), b"keep")

    def test_symlink_input_is_refused(self) -> None:
        target = self.root / "target"
        target.write_bytes(b"data")
        link = self.root / "link"
        link.symlink_to(target)
        with self.assertRaisesRegex(ValueError, "regular file"):
            PACKAGER.require_regular(link, "test input")

    def test_output_is_deterministic(self) -> None:
        staged = self.root / "linux"
        (staged / PACKAGER.RELEASE).mkdir(parents=True)
        (staged / "install.sh").write_text("#!/bin/sh\n")
        (staged / PACKAGER.RELEASE / PACKAGER.LINUX_ARTIFACT).write_bytes(elf())
        first = self.root / "first.tar.gz"
        second = self.root / "second.tar.gz"
        self.package("linux-x86_64", staged, first)
        self.package("linux-x86_64", staged, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())


if __name__ == "__main__":
    unittest.main(verbosity=2)
