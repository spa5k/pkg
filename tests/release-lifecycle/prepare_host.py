#!/usr/bin/env python3
"""Restore system-owned install directories only on a disposable hosted CI image."""
import os
from pathlib import Path
import stat
import subprocess

PREFIXES = (Path("/opt"), Path("/usr/local"), Path("/usr/local/bin"))


def prepare():
    if (os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or os.geteuid() == 0):
        raise ValueError("requires a normal user on a disposable GitHub-hosted runner")
    if os.path.lexists("/usr/local/bin/pkg") or os.path.lexists("/nix/receipt.json"):
        raise ValueError("existing installations must be retained")
    # Validate every directory before the first change. Never follow symlinks.
    for path in PREFIXES:
        metadata = path.lstat()
        if not stat.S_ISDIR(metadata.st_mode):
            raise ValueError(f"not a real directory: {path}")
        print(f"Before: {path} uid={metadata.st_uid} gid={metadata.st_gid} "
              f"mode={stat.S_IMODE(metadata.st_mode):04o}", flush=True)
    # Hosted tool images can make these directories writable by the runner.
    # Use the normal OS ownership contract. Do not change any child files.
    for path in PREFIXES:
        subprocess.run(["sudo", "chown", "0:0", str(path)], check=True)
        subprocess.run(["sudo", "chmod", "0755", str(path)], check=True)
        metadata = path.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != 0
                or metadata.st_gid != 0 or stat.S_IMODE(metadata.st_mode) != 0o755):
            raise ValueError(f"system directory preparation failed: {path}")
        print(f"Ready: {path} uid=0 gid=0 mode=0755", flush=True)


if __name__ == "__main__":
    prepare()
