"""Real renderer refusals and shell setup. Bootstrap execution owns digest proof."""
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TAG = "v0.1.0-alpha.49"
LINUX_ARTIFACT = b"#!/bin/sh\npkg x86-64 linux artifact fixture bytes\n"
MACOS_PACKAGE = b"pkg macOS preview package fixture bytes for 0.1.0-alpha.49\n"
MACOS_WRAPPER = b"#!/bin/sh\npkg macOS preview wrapper fixture bytes\n"
ARTIFACTS = {
    "pkg-install-x86_64-linux": LINUX_ARTIFACT,
    f"pkg-{TAG[1:]}-preview.pkg": MACOS_PACKAGE,
    "install-preview.sh": MACOS_WRAPPER,
}


def write_assets(root: Path) -> None:
    """Write a complete fixture whose three artifact payloads are pairwise distinct."""
    for name, data in ARTIFACTS.items():
        (root / name).write_bytes(data)


def run_render_cli(tag: str, assets: Path, output: Path) -> subprocess.CompletedProcess:
    """Invoke the real render.py CLI the release flow uses."""
    return subprocess.run(
        [sys.executable, str(Path(__file__).with_name("render.py")), "--", tag, str(assets), str(output)],
        capture_output=True, text=True, timeout=10,
    )


class RenderedReleaseProof(unittest.TestCase):
    def test_each_unsafe_artifact_refuses_at_the_cli(self):
        for name in ARTIFACTS:
            for label in ["missing", "symlink", "empty", "directory"]:
                with self.subTest(artifact=name, case=label):
                    with tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        write_assets(root)
                        target = root / name
                        if label == "missing":
                            target.unlink()
                        elif label == "symlink":
                            decoy = root / "decoy-payload"
                            decoy.write_bytes(b"nonempty decoy bytes\n")
                            target.unlink()
                            target.symlink_to(decoy)
                        elif label == "empty":
                            target.write_bytes(b"")
                        else:
                            target.unlink()
                            target.mkdir()
                        output = root / "rendered-install.sh"
                        result = run_render_cli(TAG, root, output)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(f"missing or unsafe release artifact: {name}", result.stderr)
                        self.assertFalse(output.exists())

    def test_invalid_tags_fail_at_the_tag_guard(self):
        unsafe_tags = [
            "v1.0.0'; echo injected",
            "$(id)",
            "v1.0.0`id`",
            "../v1.0.0",
            "v1.0.0\nrm -rf /",
            "1.0.0",
            "v1.0.0-alpha.0",
        ]
        for tag in unsafe_tags:
            with self.subTest(tag=tag):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    write_assets(root)
                    # A disabled tag guard would reach this tag-derived artifact
                    # name; create it safely when it is one plain path component
                    # so only the tag guard can refuse the run. These fixture
                    # files are never executed.
                    derived = f"pkg-{tag[1:]}-preview.pkg"
                    if "/" not in derived and "\x00" not in derived:
                        (root / derived).write_bytes(b"fixture for a disabled tag guard\n")
                    output = root / "rendered-install.sh"
                    result = run_render_cli(tag, root, output)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("invalid release tag", result.stderr)
                    self.assertFalse(output.exists())


class RealShellBoundaryTests(unittest.TestCase):
    def test_guarded_startup_survives_removed_binary(self):
        source = (ROOT / "docs/install.sh").read_text()
        snippet = next(line for line in source.split("'") if line.startswith("  [ ! -x /usr/local/bin/pkg ]"))
        wrapper = (ROOT / "packaging/macos/install-preview.sh").read_text()
        self.assertIn("echo '" + snippet + "'", wrapper)
        self.assertIn("add " + snippet.strip() + " to ~/.zshrc.", wrapper)
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "pkg"
            binary.write_text('#!/bin/sh\nprintf "export PKG_SHELL_TEST=ready\\n"\n')
            binary.chmod(0o700)
            snippet = snippet.replace("/usr/local/bin/pkg", shlex.quote(str(binary)))
            for shell in ["/bin/bash", "/bin/zsh"]:
                if not Path(shell).exists():
                    continue
                installed = subprocess.run([shell, "-c", snippet + '; test "$PKG_SHELL_TEST" = ready'], capture_output=True)
                self.assertEqual(installed.returncode, 0, installed.stderr)
            binary.unlink()
            for shell in ["/bin/bash", "/bin/zsh"]:
                if Path(shell).exists():
                    removed = subprocess.run([shell, "-c", snippet], capture_output=True)
                    self.assertEqual(removed.returncode, 0, removed.stderr)
                    self.assertEqual(removed.stderr, b"")


if __name__ == "__main__":
    unittest.main()
