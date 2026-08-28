#!/usr/bin/env python3
"""Build a deterministic, local-only alpha candidate archive."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import pathlib
import re
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import xml.etree.ElementTree as etree


LINUX_ARTIFACT = "pkg-installer-x86_64-linux"
MACOS_INSTALLER = "pkg-install"
ALPHA_RELEASE = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+-alpha\.[1-9][0-9]*$")
NIX_SOURCE_SHA256 = "ecc2f226a1ba27ad56eb85f42af8f078067fe5a219fceb82cb3fda9ba24387a5"
NIX_SOURCE_MEMBER = "nix-2.34.8/COPYING"
PROJECT_ROOT = pathlib.Path(__file__).resolve().parents[2]
MIN_INSTALLER_SIZE = 1024 * 1024
MIN_PACKAGE_SIZE = 1024 * 1024

def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def require_release(value: str) -> str:
    if ALPHA_RELEASE.fullmatch(value) is None:
        raise ValueError("release must be an exact alpha tag")
    return value


def release_version(release: str) -> str:
    return require_release(release).removeprefix("v")


def macos_package(release: str) -> str:
    return f"pkg-{release_version(release)}-preview.pkg"


def require_regular(path: pathlib.Path, name: str) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ValueError(f"{name} must be a regular file") from error
    with os.fdopen(descriptor, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise ValueError(f"{name} must be a regular file")
        return source.read()


def require_linux_installer(data: bytes) -> None:
    if (
        len(data) < MIN_INSTALLER_SIZE
        or data[:4] != b"\x7fELF"
        or data[4] != 2
        or data[5] != 1
        or struct.unpack_from("<H", data, 16)[0] not in (2, 3)
        or struct.unpack_from("<H", data, 18)[0] != 62
        or struct.unpack_from("<I", data, 20)[0] != 1
        or struct.unpack_from("<H", data, 52)[0] != 64
        or struct.unpack_from("<H", data, 54)[0] != 56
    ):
        raise ValueError("pkg-install is not a 64-bit little-endian x86-64 ELF")
    program_offset = struct.unpack_from("<Q", data, 32)[0]
    program_count = struct.unpack_from("<H", data, 56)[0]
    if program_count == 0 or program_offset + program_count * 56 > len(data):
        raise ValueError("pkg-install has an invalid ELF program table")
    executable_load = False
    for index in range(program_count):
        offset = program_offset + index * 56
        kind, flags = struct.unpack_from("<II", data, offset)
        file_offset = struct.unpack_from("<Q", data, offset + 8)[0]
        file_size = struct.unpack_from("<Q", data, offset + 32)[0]
        if file_offset + file_size > len(data):
            raise ValueError("pkg-install has an invalid ELF segment")
        executable_load |= kind == 1 and flags & 1 != 0 and file_size != 0
    if not executable_load:
        raise ValueError("pkg-install has no executable ELF load segment")


def require_macos_installer(data: bytes) -> None:
    if (
        len(data) < MIN_INSTALLER_SIZE
        or struct.unpack_from("<I", data, 0)[0] != 0xFEEDFACF
        or struct.unpack_from("<I", data, 4)[0] != 0x0100000C
        or struct.unpack_from("<I", data, 12)[0] != 2
    ):
        raise ValueError("pkg-install is not a thin 64-bit arm64 Mach-O")
    command_count, command_bytes = struct.unpack_from("<II", data, 16)
    if command_count == 0 or command_bytes == 0 or 32 + command_bytes > len(data):
        raise ValueError("pkg-install has an invalid Mach-O load command table")
    offset = 32
    executable_text = entrypoint = False
    for _ in range(command_count):
        if offset + 8 > 32 + command_bytes:
            raise ValueError("pkg-install has a truncated Mach-O load command")
        command, size = struct.unpack_from("<II", data, offset)
        if size < 8 or offset + size > 32 + command_bytes:
            raise ValueError("pkg-install has an invalid Mach-O load command")
        if command == 0x19 and size >= 72:
            segment = data[offset + 8 : offset + 24].rstrip(b"\0")
            file_offset, file_size = struct.unpack_from("<QQ", data, offset + 40)
            initial_protection = struct.unpack_from("<I", data, offset + 60)[0]
            executable_text |= (
                segment == b"__TEXT"
                and file_size != 0
                and file_offset + file_size <= len(data)
                and initial_protection & 4 != 0
            )
        entrypoint |= command in (5, 0x80000028)
        offset += size
    if offset != 32 + command_bytes or not executable_text or not entrypoint:
        raise ValueError("pkg-install is not an executable Mach-O image")


def validate_expanded_macos_package(
    expanded: pathlib.Path, installer: bytes, version: str
) -> None:
    expected = {"PackageInfo", "Scripts", "Scripts/pkg-install", "Scripts/postinstall"}
    actual = set()
    for path in expanded.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError("macOS preview package has an unsafe expanded file")
        actual.add(path.relative_to(expanded).as_posix())
    if actual != expected:
        raise ValueError("macOS preview package has unexpected expanded files")
    embedded_installer = require_regular(
        expanded / "Scripts/pkg-install", "embedded pkg-install"
    )
    if embedded_installer != installer:
        raise ValueError("macOS preview package contains a different pkg-install")
    require_macos_installer(embedded_installer)
    postinstall = require_regular(
        expanded / "Scripts/postinstall", "embedded postinstall"
    )
    if postinstall != require_regular(
        PROJECT_ROOT / "packaging/macos/postinstall", "fixed postinstall"
    ):
        raise ValueError("macOS preview package contains a different postinstall")
    try:
        package_info = etree.fromstring(
            require_regular(expanded / "PackageInfo", "PackageInfo")
        )
    except etree.ParseError as error:
        raise ValueError("macOS preview package has invalid PackageInfo") from error
    postinstall = package_info.find("./scripts/postinstall")
    if (
        package_info.tag != "pkg-info"
        or package_info.get("identifier") != "org.pkg.installer.preview"
        or package_info.get("version") != version
        or package_info.get("auth") != "root"
        or postinstall is None
        or postinstall.get("file") != "./postinstall"
    ):
        raise ValueError("macOS preview package has unexpected PackageInfo")


def require_macos_package(data: bytes, installer: bytes, release: str) -> None:
    version = release_version(release)
    if len(data) < MIN_PACKAGE_SIZE or data[:4] != b"xar!":
        raise ValueError("macOS preview package is not a XAR archive")
    if sys.platform != "darwin":
        raise ValueError("macOS preview packages must be validated on macOS")
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        executable = root / MACOS_INSTALLER
        executable.write_bytes(installer)
        executable.chmod(0o700)
        try:
            subprocess.run(
                [
                    "/usr/bin/codesign",
                    "--verify",
                    "--strict",
                    "--verbose=2",
                    str(executable),
                ],
                check=True,
                capture_output=True,
            )
            signature = subprocess.run(
                [
                    "/usr/bin/codesign",
                    "--display",
                    "--verbose=4",
                    str(executable),
                ],
                check=True,
                capture_output=True,
                text=True,
            )
        except subprocess.CalledProcessError as error:
            raise ValueError("pkg-install has an invalid code signature") from error
        if "Signature=adhoc" not in signature.stderr.splitlines():
            raise ValueError("pkg-install does not have an ad-hoc code signature")
        package = root / macos_package(release)
        package.write_bytes(data)
        expanded = root / "expanded"
        try:
            subprocess.run(
                ["/usr/sbin/pkgutil", "--expand-full", str(package), str(expanded)],
                check=True,
                capture_output=True,
            )
        except subprocess.CalledProcessError as error:
            raise ValueError("macOS preview package cannot be expanded") from error
        validate_expanded_macos_package(expanded, installer, version)


def require_nix_copying(source: pathlib.Path) -> bytes:
    data = require_regular(source, "Nix source archive")
    if sha256_bytes(data) != NIX_SOURCE_SHA256:
        raise ValueError("Nix 2.34.8 source archive checksum does not match")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        try:
            member = archive.getmember(NIX_SOURCE_MEMBER)
        except KeyError as error:
            raise ValueError("Nix 2.34.8 source archive has no COPYING file") from error
        if not member.isfile() or member.size > 256 * 1024:
            raise ValueError("Nix 2.34.8 COPYING entry is not a small regular file")
        extracted = archive.extractfile(member)
        if extracted is None:
            raise ValueError("Nix 2.34.8 COPYING entry cannot be read")
        copying = extracted.read()
    if b"GNU LESSER GENERAL PUBLIC LICENSE" not in copying:
        raise ValueError("Nix 2.34.8 COPYING entry has unexpected content")
    return copying


def release_notes(
    platform: str, release: str, published_preview: bool = False
) -> bytes:
    release = require_release(release)
    package = macos_package(release)
    if published_preview:
        release_url = f"https://github.com/spa5k/pkg/releases/download/{release}"
        return (
            f"# pkg {release} technical preview\n\n"
            "This preview uses signed release metadata.\n\n"
            "## Linux x86-64\n\n"
            "```sh\n"
            f"curl -fsSLO {release_url}/install.sh\n"
            "less install.sh\n"
            "sh install.sh\n"
            "```\n\n"
            "## macOS Apple silicon\n\n"
            "The package has an ad-hoc signature. It is not notarized.\n\n"
            "```sh\n"
            f"curl -fsSLO {release_url}/{package}\n"
            f"curl -fsSLO {release_url}/SHA256SUMS\n"
            f"grep '  {package}$' SHA256SUMS | shasum -a 256 --check\n"
            f"sudo installer -pkg ./{package} -target /\n"
            "```\n\n"
            "## Downloads\n\n"
            f"- [Linux installer]({release_url}/install.sh)\n"
            f"- [Linux archive]({release_url}/pkg-{release}-linux-x86_64.tar.gz)\n"
            f"- [macOS package]({release_url}/{package})\n"
            f"- [macOS archive]({release_url}/pkg-{release}-macos-aarch64.tar.gz)\n"
            f"- [Checksums]({release_url}/SHA256SUMS)\n\n"
            "This is a preview release. It is not the v1 release.\n"
        ).encode()
    install = (
        "Run `sh install.sh` only while the local proof service is active."
        if platform == "linux-x86_64"
        else f"Run `sudo installer -pkg {release}/{package} -target /` in the disposable test host."
    )
    macos_limit = (
        "\nThe embedded pkg-install uses an ad-hoc signature.\n"
        "It is not notarized.\n"
        "Developer ID signing and notarization are TODO items.\n"
        if platform == "macos-aarch64"
        else "\n"
    )
    return (
        f"# pkg {release} local candidate\n\n"
        "TEST KEYS. LOOPBACK SERVICE. NOT FOR PUBLICATION.\n\n"
        "This archive is for local product proof only.\n"
        "The installer trusts test TUF keys and fixed loopback URLs.\n"
        f"{install}\n"
        f"{macos_limit}"
        "Production TUF signing and fixed HTTPS hosting are still required.\n"
    ).encode()


def nix_source_notice() -> bytes:
    return b"""# Nix 2.34.8 source information

pkg uses Nix 2.34.8 as its managed runtime.

Source tag: https://github.com/NixOS/nix/tree/2.34.8
Source commit: f3f1c3c5b8ad91850e0f7c590cf177f7ab022024
Source archive: https://github.com/NixOS/nix/archive/refs/tags/2.34.8.tar.gz
Source archive SHA-256: ecc2f226a1ba27ad56eb85f42af8f078067fe5a219fceb82cb3fda9ba24387a5

Linux binary archive: https://releases.nixos.org/nix/nix-2.34.8/nix-2.34.8-x86_64-linux.tar.xz
Linux binary SHA-256: 2c2e146b80834fe0ca201b51deeb939405b4f18e8d2071bf80b10f8123c50464

macOS binary archive: https://releases.nixos.org/nix/nix-2.34.8/nix-2.34.8-aarch64-darwin.tar.xz
macOS binary SHA-256: ae3b2b1a74b956110d14dd813bee80ea46626a51ddce28d142e0805379a34acf
"""


def require_third_party(data: bytes) -> bytes:
    if (
        b"Third-Party Licenses" not in data
        or b'data-crate="' not in data
        or b"<pre>" not in data
    ):
        raise ValueError("third-party license report has unexpected content")
    return data


def generate_third_party(cargo_about: pathlib.Path) -> bytes:
    require_regular(cargo_about, "cargo-about executable")
    version = subprocess.run(
        [str(cargo_about), "--version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if version != "cargo-about 0.9.1":
        raise ValueError("cargo-about 0.9.1 is required")
    with tempfile.TemporaryDirectory() as directory:
        output = pathlib.Path(directory) / "THIRD_PARTY_LICENSES.html"
        subprocess.run(
            [
                str(cargo_about),
                "generate",
                "--frozen",
                "--fail",
                "--workspace",
                "--config",
                str(pathlib.Path(__file__).with_name("about.toml")),
                "--output-file",
                str(output),
                str(pathlib.Path(__file__).with_name("about.hbs")),
            ],
            cwd=PROJECT_ROOT,
            check=True,
        )
        return require_third_party(require_regular(output, "third-party license report"))


def write_archive(files: dict[str, tuple[bytes, int]], output: pathlib.Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() or output.is_symlink():
        raise ValueError("candidate archive already exists")
    directories = sorted(
        {str(parent) for name in files for parent in pathlib.PurePosixPath(name).parents if str(parent) != "."}
    )
    with output.open("xb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for directory in directories:
                    info = tarfile.TarInfo(directory)
                    info.type = tarfile.DIRTYPE
                    info.mode = 0o755
                    info.mtime = info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    archive.addfile(info)
                for name in sorted(files):
                    data, mode = files[name]
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    info.mode = mode
                    info.mtime = info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    archive.addfile(info, fileobj=io.BytesIO(data))


def package_linux_candidate(
    release: str,
    staged: pathlib.Path,
    project_license: pathlib.Path,
    cargo_about: pathlib.Path,
    output: pathlib.Path,
    published_preview: bool = False,
) -> None:
    release = require_release(release)
    platform_files = ("install.sh", f"{release}/{LINUX_ARTIFACT}")
    payload: dict[str, tuple[bytes, int]] = {}
    for name in platform_files:
        data = require_regular(staged / name, name)
        payload[name] = (data, 0o755)
    require_linux_installer(payload[f"{release}/{LINUX_ARTIFACT}"][0])

    license_text = require_regular(project_license, "Apache-2.0 license")
    if b"Apache License\n                           Version 2.0" not in license_text:
        raise ValueError("project license is not Apache-2.0")
    notices = generate_third_party(cargo_about)
    payload.update(
        {
            "LICENSE": (license_text, 0o644),
            "RELEASE_NOTES.md": (
                release_notes("linux-x86_64", release, published_preview),
                0o644,
            ),
            "THIRD_PARTY_LICENSES.html": (notices, 0o644),
        }
    )
    checksums = "".join(
        f"{sha256_bytes(data)}  {name}\n" for name, (data, _) in sorted(payload.items())
    ).encode("ascii")
    payload["SHA256SUMS"] = (checksums, 0o644)
    write_archive(payload, output)


def package_macos_candidate(
    release: str,
    staged: pathlib.Path,
    project_license: pathlib.Path,
    cargo_about: pathlib.Path,
    nix_source: pathlib.Path,
    output: pathlib.Path,
    published_preview: bool = False,
) -> None:
    release = require_release(release)
    package = macos_package(release)
    platform_files = (f"{release}/{MACOS_INSTALLER}", f"{release}/{package}")
    payload: dict[str, tuple[bytes, int]] = {}
    for name in platform_files:
        data = require_regular(staged / name, name)
        payload[name] = (
            data,
            0o755
            if name.endswith(("install.sh", "pkg-install", LINUX_ARTIFACT))
            else 0o644,
        )

    installer = payload[f"{release}/{MACOS_INSTALLER}"][0]
    require_macos_installer(installer)
    require_macos_package(payload[f"{release}/{package}"][0], installer, release)

    license_text = require_regular(project_license, "Apache-2.0 license")
    if b"Apache License\n                           Version 2.0" not in license_text:
        raise ValueError("project license is not Apache-2.0")
    notices = generate_third_party(cargo_about)

    payload.update(
        {
            "LICENSE": (license_text, 0o644),
            "NIX-LICENSE": (require_nix_copying(nix_source), 0o644),
            "NIX-SOURCE.md": (nix_source_notice(), 0o644),
            "RELEASE_NOTES.md": (
                release_notes("macos-aarch64", release, published_preview),
                0o644,
            ),
            "THIRD_PARTY_LICENSES.html": (notices, 0o644),
        }
    )
    checksums = "".join(
        f"{sha256_bytes(data)}  {name}\n" for name, (data, _) in sorted(payload.items())
    ).encode("ascii")
    payload["SHA256SUMS"] = (checksums, 0o644)
    write_archive(payload, output)


def main() -> int:
    parser = argparse.ArgumentParser()
    modes = parser.add_subparsers(dest="platform", required=True)
    linux = modes.add_parser("linux-x86_64")
    linux.add_argument("release")
    linux.add_argument("staged", type=pathlib.Path)
    linux.add_argument("project_license", type=pathlib.Path)
    linux.add_argument("cargo_about", type=pathlib.Path)
    linux.add_argument("output", type=pathlib.Path)
    linux.add_argument("--published-preview", action="store_true")
    macos = modes.add_parser("macos-aarch64")
    macos.add_argument("release")
    macos.add_argument("staged", type=pathlib.Path)
    macos.add_argument("project_license", type=pathlib.Path)
    macos.add_argument("cargo_about", type=pathlib.Path)
    macos.add_argument("nix_source", type=pathlib.Path)
    macos.add_argument("output", type=pathlib.Path)
    macos.add_argument("--published-preview", action="store_true")
    args = parser.parse_args()
    try:
        if args.platform == "linux-x86_64":
            package_linux_candidate(
                args.release,
                args.staged,
                args.project_license,
                args.cargo_about,
                args.output,
                args.published_preview,
            )
        else:
            package_macos_candidate(
                args.release,
                args.staged,
                args.project_license,
                args.cargo_about,
                args.nix_source,
                args.output,
                args.published_preview,
            )
    except (OSError, subprocess.CalledProcessError, tarfile.TarError, ValueError) as error:
        print(f"package-alpha-candidate: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
