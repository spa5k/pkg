#!/usr/bin/env python3
"""Install a dependency-free, non-relocating bottle into one Nix output."""

import json
import os
import pathlib
import shutil
import sys
import tarfile

PREFIXES = (b"/opt/homebrew", b"/home/linuxbrew/.linuxbrew", b"/usr/local", b"@@HOMEBREW_")


def install(archive, plan, output, stage_linux_elf=False):
    root = f"{plan['name']}/{plan['cellarVersion']}"
    if "/" in plan["name"] or "/" in plan["cellarVersion"]:
        raise ValueError("invalid bottle root")
    stage = pathlib.Path("bottle-stage")
    stage.mkdir()
    with tarfile.open(archive, "r:gz") as source:
        members = []
        expanded = 0
        for member in source:
            expanded += member.size
            if len(members) >= 100000 or expanded > 2 * 1024**3:
                raise ValueError("bottle exceeds extraction limits")
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or not (member.name == plan['name'] or member.name == root or member.name.startswith(root + "/")):
                raise ValueError("bottle member escapes its Cellar root")
            if not (member.isdir() or member.isfile() or member.issym() or member.islnk()):
                raise ValueError("unsupported bottle member")
            if member.issym() or member.islnk():
                target = pathlib.PurePosixPath(member.linkname)
                if target.is_absolute():
                    raise ValueError("absolute bottle link")
                location = stage / path.parent / target if member.issym() else stage / target
                if not location.resolve().is_relative_to((stage / root).resolve()):
                    raise ValueError("bottle link escapes its package")
            members.append(member)
        source.extractall(stage, members=members, filter="data")
    payload = stage / root
    if not payload.is_dir() or payload.is_symlink():
        raise ValueError("bottle package root is missing")
    recipe = payload / ".brew"
    if recipe.is_symlink():
        recipe.unlink()
    elif recipe.exists():
        shutil.rmtree(recipe)
    (payload / "INSTALL_RECEIPT.json").unlink(missing_ok=True)
    audit(payload, stage_linux_elf)
    if not any(path.is_file() and os.access(path, os.X_OK) for path in (payload / "bin").glob("*")):
        raise ValueError("bottle has no executable in bin")
    shutil.copytree(payload, output, symlinks=True)


def audit(payload, stage_linux_elf=False):
    payload = pathlib.Path(payload)
    for path in payload.rglob("*"):
        if path.is_symlink():
            if not path.resolve().is_relative_to(payload.resolve()) or not path.resolve().exists():
                raise ValueError("bottle has an external or dangling link")
        elif path.is_file():
            with path.open("rb") as stream:
                if stage_linux_elf and stream.read(4) == b"\x7fELF":
                    continue
                stream.seek(0)
                previous = b""
                while block := stream.read(1024 * 1024):
                    data = previous + block
                    if any(prefix in data for prefix in PREFIXES):
                        raise ValueError(f"bottle still refers to Homebrew paths: {str(path.relative_to(payload))!r}")
                    previous = data[-64:]


if __name__ == "__main__":
    try:
        if sys.argv[1] == "audit":
            audit(sys.argv[2])
        else:
            install(sys.argv[1], json.loads(pathlib.Path(sys.argv[2]).read_text()), sys.argv[3],
                    stage_linux_elf=sys.argv[4:] == ["--stage-linux-elf"])
    except (OSError, ValueError, tarfile.TarError) as error:
        sys.exit(f"Formula bottle refused: {error}")
