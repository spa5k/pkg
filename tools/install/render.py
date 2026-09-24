#!/usr/bin/env python3
"""Render the public bootstrap from locally verified release artifacts."""
import argparse
import hashlib
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
BASE_URL = "https://github.com/spa5k/pkg/releases/download"


def render(tag: str, assets: Path) -> str:
    if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[1-9][0-9]*)?", tag) is None:
        raise ValueError("invalid release tag")
    replacements = {"PKG_RELEASE": tag, "PKG_RELEASE_BASE_URL": BASE_URL, "PKG_ARTIFACT_X86_64_LINUX": "pkg-install-x86_64-linux"}
    for key, name in {
        "PKG_SHA256_X86_64_LINUX": "pkg-install-x86_64-linux",
        "PKG_SHA256_MACOS_PACKAGE": f"pkg-{tag[1:]}-preview.pkg",
        "PKG_SHA256_MACOS_WRAPPER": "install-preview.sh",
    }.items():
        path = assets / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"missing or unsafe release artifact: {name}")
        with path.open("rb") as stream:
            replacements[key] = hashlib.file_digest(stream, "sha256").hexdigest()
    source = (ROOT / "docs/install.sh").read_text()
    for key, value in replacements.items():
        source = source.replace(f"@{key}@", value)
    if re.search(r"@PKG_[A-Z0-9_]+@", source):
        raise ValueError("unfilled release template")
    return source


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("assets", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(render(args.tag, args.assets))
