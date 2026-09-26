#!/usr/bin/env python3
"""Run the BUG-01 assertion against the current repository's broker library.

The unfixed implementation exits 101. A fixed implementation should exit 0.
This uses in-process broker objects. It does not connect to installed services.
"""

import json
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    evidence = Path(__file__).resolve().parent
    repo = Path(__file__).resolve().parents[4]
    build = subprocess.run(
        ["rustup", "run", "1.96.1", "cargo", "build", "--locked", "-p", "pkg-nix",
         "--message-format=json"],
        cwd=repo, capture_output=True, text=True,
    )
    sys.stderr.write(build.stderr)
    if build.returncode:
        sys.stdout.write(build.stdout)
        return build.returncode
    libraries = []
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if (message.get("reason") == "compiler-artifact"
                and message.get("target", {}).get("name") == "pkg_nix"):
            libraries.extend(Path(name) for name in message["filenames"]
                             if name.endswith(".rlib"))
    if len(libraries) != 1:
        raise RuntimeError(f"Expected one pkg_nix library, found {libraries}")
    dependencies = libraries[0].parent
    if dependencies.name != "deps":
        dependencies = dependencies / "deps"
    with tempfile.TemporaryDirectory(prefix="pkg-broker-repro-") as temporary:
        binary = Path(temporary) / "broker-session-probe"
        subprocess.run(
            ["rustup", "run", "1.96.1", "rustc", "--edition=2024",
             str(evidence / "broker_session_probe.rs"),
             "--extern", f"pkg_nix={libraries[0]}",
             "-L", f"dependency={dependencies}", "-o", str(binary)],
            cwd=repo, check=True,
        )
        return subprocess.run([str(binary)], cwd=repo).returncode


if __name__ == "__main__":
    sys.exit(main())
