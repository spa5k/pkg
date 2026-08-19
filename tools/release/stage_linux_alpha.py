#!/usr/bin/env python3
"""Stage the fixed Linux alpha installer and render its bootstrap script."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import re
import shutil
import struct
import sys
from urllib.parse import urlsplit


RELEASE = "v0.1.0-alpha.3"
ARTIFACT = "pkg-installer-x86_64-linux"


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require_x86_64_linux_elf(path: pathlib.Path) -> None:
    with path.open("rb") as source:
        header = source.read(20)
    if (
        len(header) != 20
        or header[:4] != b"\x7fELF"
        or header[4] != 2
        or header[5] != 1
        or struct.unpack_from("<H", header, 18)[0] != 62
    ):
        raise ValueError("pkg-install is not a 64-bit little-endian x86-64 ELF")


def require_https_base_url(value: str) -> str:
    parsed = urlsplit(value)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or any(character in value for character in "'\"\\\r\n")
    ):
        raise ValueError("release base URL must be a plain HTTPS origin or path")
    return value.rstrip("/")


def stage(
    pkg_install: pathlib.Path,
    template: pathlib.Path,
    destination: pathlib.Path,
    base_url: str,
) -> None:
    require_x86_64_linux_elf(pkg_install)
    base_url = require_https_base_url(base_url)
    destination.mkdir(parents=True)
    release_dir = destination / RELEASE
    release_dir.mkdir()
    artifact = release_dir / ARTIFACT
    shutil.copyfile(pkg_install, artifact)
    artifact.chmod(0o755)

    replacements = {
        "@PKG_RELEASE@": RELEASE,
        "@PKG_RELEASE_BASE_URL@": base_url,
        "@PKG_SHA256_X86_64_LINUX@": sha256(artifact),
    }
    rendered = template.read_text(encoding="utf-8")
    for token, value in replacements.items():
        if rendered.count(token) != 1:
            raise ValueError(f"installer template must contain {token} exactly once")
        rendered = rendered.replace(token, value)
    if re.search(r"@PKG_[A-Z0-9_]+@", rendered):
        raise ValueError("installer template contains an unresolved release token")
    bootstrap = destination / "install.sh"
    bootstrap.write_text(rendered, encoding="utf-8")
    bootstrap.chmod(0o755)

    checksums = destination / "SHA256SUMS"
    checksums.write_text(
        f"{sha256(artifact)}  {RELEASE}/{ARTIFACT}\n"
        f"{sha256(bootstrap)}  install.sh\n",
        encoding="ascii",
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("pkg_install", type=pathlib.Path)
    parser.add_argument("template", type=pathlib.Path)
    parser.add_argument("destination", type=pathlib.Path)
    parser.add_argument("release_base_url")
    args = parser.parse_args()
    try:
        stage(args.pkg_install, args.template, args.destination, args.release_base_url)
    except (OSError, ValueError) as error:
        print(f"stage-linux-alpha: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
